//! Log downloads and support diagnostics API handlers.

use super::error;
use crate::state::Shared;
use axum::{
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

pub(super) fn handle(h: &Shared, method: &str, path: &str, data: &Value) -> anyhow::Result<Value> {
    Ok(match (method, path) {
        ("GET", "/api/health/crashdump") => crate::maintenance::crash_status(h)?,
        ("POST", "/api/health/crashdump/dismiss") => crate::maintenance::dismiss_crash(h, data)?,
        ("GET", "/api/logs/export_crash/manifest") => crate::maintenance::bundle_manifest(h),
        _ => return Err(anyhow::anyhow!("unknown API endpoint")),
    })
}

pub(super) async fn export_crash(h: &Shared) -> Response {
    let h = h.clone();
    let result = tokio::task::spawn_blocking(move || -> anyhow::Result<std::fs::File> {
        use std::os::windows::fs::OpenOptionsExt;
        let path = crate::maintenance::bundle(&h)?;
        Ok(std::fs::OpenOptions::new()
            .access_mode(0x80000000 | 0x00010000)
            .share_mode(1 | 4)
            .custom_flags(0x04000000)
            .open(path)?)
    })
    .await;
    match result {
        Ok(Ok(file)) => (
            [
                (header::CONTENT_TYPE, "application/zip"),
                (
                    header::CONTENT_DISPOSITION,
                    "attachment; filename=butterpollo-support.zip",
                ),
            ],
            axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(
                tokio::fs::File::from_std(file),
            )),
        )
            .into_response(),
        Ok(Err(err)) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
        Err(err) => error(StatusCode::INTERNAL_SERVER_ERROR, &err.to_string()),
    }
}

pub(super) fn tail(h: &Shared, uri: &axum::http::Uri) -> Response {
    use std::io::{Read, Seek};
    let query: std::collections::HashMap<String, String> =
        url::form_urlencoded::parse(uri.query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let offset: i64 = query
        .get("offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(-1);
    let max: u64 = query
        .get("max")
        .and_then(|v| v.parse().ok())
        .unwrap_or(256 * 1024)
        .clamp(1024, 4 * 1024 * 1024);
    let result = (|| -> std::io::Result<Value> {
        let mut file = match std::fs::File::open(crate::maintenance::log_path(h)) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(
                    json!({"status":true,"offset":0,"size":0,"reset":offset != 0,"text":""}),
                );
            }
            Err(e) => return Err(e),
        };
        let size = file.metadata()?.len();
        // A negative offset asks for the tail; an offset past the end
        // means the log was rotated.
        let reset = offset < 0 || offset as u64 > size;
        let start = if reset {
            size.saturating_sub(max)
        } else {
            offset as u64
        };
        file.seek(std::io::SeekFrom::Start(start))?;
        let mut bytes = vec![];
        file.take(max).read_to_end(&mut bytes)?;
        // End on a line break so no line is split between two reads.
        if start + (bytes.len() as u64) < size
            && let Some(end) = bytes.iter().rposition(|b| *b == b'\n')
        {
            bytes.truncate(end + 1);
        }
        Ok(json!({
            "status": true,
            "offset": start + bytes.len() as u64,
            "size": size,
            "reset": reset,
            "text": String::from_utf8_lossy(&bytes),
        }))
    })();
    match result {
        Ok(value) => axum::Json(value).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

pub(super) fn export(h: &Shared, path: &str) -> Response {
    use std::io::{Read, Seek};
    let result = (|| -> std::io::Result<String> {
        let mut file = std::fs::File::open(crate::maintenance::log_path(h))?;
        let length = file.metadata()?.len();
        file.seek(std::io::SeekFrom::Start(
            length.saturating_sub(8 * 1024 * 1024),
        ))?;
        let mut bytes = vec![];
        file.take(8 * 1024 * 1024).read_to_end(&mut bytes)?;
        Ok(String::from_utf8_lossy(&bytes).into_owned())
    })();
    let mut result = (
        [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
        result.unwrap_or_default(),
    )
        .into_response();
    if path.ends_with("/export") {
        result.headers_mut().insert(
            header::CONTENT_DISPOSITION,
            "attachment; filename=butterpollo.log".parse().unwrap(),
        );
    }
    result
}
