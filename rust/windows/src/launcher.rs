//! First-run and repeated-launch experience for the portable Windows package.
use anyhow::{Context, Result, bail};
use butterpollo_core::{config::Config, migration};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpStream},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::Command,
    time::{Duration, Instant},
};
use windows::{
    Win32::{
        System::Com::*,
        UI::{Shell::*, WindowsAndMessaging::*},
    },
    core::{PCWSTR, w},
};
fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
fn folder() -> Result<Option<PathBuf>> {
    unsafe {
        CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()?;
        struct Com;
        impl Drop for Com {
            fn drop(&mut self) {
                unsafe { CoUninitialize() }
            }
        }
        let _com = Com;
        let dialog: IFileOpenDialog =
            CoCreateInstance(&FileOpenDialog, None, CLSCTX_INPROC_SERVER)?;
        dialog.SetOptions(
            dialog.GetOptions()? | FOS_PICKFOLDERS | FOS_FORCEFILESYSTEM | FOS_PATHMUSTEXIST,
        )?;
        dialog.SetTitle(w!("Select the Vibepollo or Apollo config folder"))?;
        if let Err(error) = dialog.Show(None) {
            if error.code() == windows::core::HRESULT(0x800704c7u32 as i32) {
                return Ok(None);
            }
            return Err(error.into());
        }
        let item = dialog.GetResult()?;
        let value = item.GetDisplayName(SIGDN_FILESYSPATH)?;
        let path = value.to_string();
        CoTaskMemFree(Some(value.0.cast()));
        Ok(Some(PathBuf::from(path?)))
    }
}
pub fn show_error(error: &str) {
    let text = wide(error);
    unsafe {
        MessageBoxW(
            None,
            PCWSTR(text.as_ptr()),
            w!("Butterpollo"),
            MB_OK | MB_ICONERROR,
        );
    }
}
fn status(port: u16) -> Option<String> {
    let address = SocketAddr::from(([127, 0, 0, 1], port));
    let mut socket = TcpStream::connect_timeout(&address, Duration::from_millis(200)).ok()?;
    socket
        .set_read_timeout(Some(Duration::from_millis(400)))
        .ok()?;
    socket
        .set_write_timeout(Some(Duration::from_millis(400)))
        .ok()?;
    socket
        .write_all(b"GET /serverinfo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
        .ok()?;
    let mut text = String::new();
    socket.take(65536).read_to_string(&mut text).ok()?;
    Some(text)
}
fn service_profile() -> Result<Option<PathBuf>> {
    use windows_service::{
        service::{ServiceAccess, ServiceState},
        service_manager::{ServiceManager, ServiceManagerAccess},
    };
    let manager = match ServiceManager::local_computer(None::<&str>, ServiceManagerAccess::CONNECT)
    {
        Ok(manager) => manager,
        Err(_) => return Ok(None),
    };
    let service = match manager.open_service(
        crate::service::NAME,
        ServiceAccess::QUERY_CONFIG | ServiceAccess::QUERY_STATUS,
    ) {
        Ok(service) => service,
        Err(_) => return Ok(None),
    };
    let installed = service.query_config()?.executable_path;
    let expected = std::env::current_exe()?
        .parent()
        .context("package directory unavailable")?
        .join("butterpollo-service.exe");
    if installed
        .to_string_lossy()
        .trim_matches('"')
        .eq_ignore_ascii_case(&expected.to_string_lossy())
    {
        if service.query_status()?.current_state != ServiceState::Running {
            bail!(
                "The Butterpollo service is installed but stopped. Start Butterpollo Rust in Windows Services, then open Start Butterpollo.exe again."
            );
        }
        return Ok(Some(
            PathBuf::from(std::env::var_os("PROGRAMDATA").context("PROGRAMDATA is unavailable")?)
                .join("Butterpollo/config"),
        ));
    }
    Ok(None)
}
pub fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut directory = migration::default_directory()?;
    let mut import = None;
    let mut interactive = true;
    while let Some(arg) = args.next() {
        if arg == "--config-dir" {
            directory = PathBuf::from(args.next().context("config directory required")?);
            interactive = false;
        } else if arg == "--import-config" {
            import = Some(PathBuf::from(
                args.next().context("existing profile folder required")?,
            ));
            interactive = false;
        } else {
            bail!("unknown start option: {}", arg.to_string_lossy());
        }
    }
    if interactive && let Some(service) = service_profile()? {
        directory = service;
        interactive = false;
    }
    let has_profile = directory.is_dir() && std::fs::read_dir(&directory)?.next().is_some();
    if !has_profile && import.is_none() && interactive {
        let choice = unsafe {
            MessageBoxW(
                None,
                w!(
                    "Bring your existing Vibepollo or Apollo settings, paired devices and library?\n\nYes: choose your old config folder and copy it into a new Butterpollo profile.\nNo: start with a new profile.\n\nThe original profile is kept."
                ),
                w!("Welcome to Butterpollo"),
                MB_YESNOCANCEL | MB_ICONQUESTION,
            )
        };
        if choice == IDCANCEL {
            return Ok(());
        }
        if choice == IDYES {
            let Some(mut source) = folder()? else {
                return Ok(());
            };
            if !source.join("sunshine.conf").is_file()
                && source.join("config/sunshine.conf").is_file()
            {
                source = source.join("config");
            }
            import = Some(source);
        }
    }
    if let Some(source) = import {
        migration::import(&source, &directory)?;
    }
    std::fs::create_dir_all(&directory)?;
    let config = Config::load(&directory.join("sunshine.conf"))?;
    let ports = config.ports()?;
    let marker = format!(
        "<RustHostProfile>{}</RustHostProfile>",
        migration::profile_id(&directory)
    );
    if let Some(response) = status(ports.http) {
        if response.contains(&marker) {
            return crate::tray::open_web(ports.web);
        }
        bail!(
            "Another streaming host is using port {}. Close Vibepollo or Apollo before starting Butterpollo, or choose a different base port in sunshine.conf. Your settings have been preserved.",
            ports.http
        );
    }
    let executable = std::env::current_exe()?
        .parent()
        .context("package directory unavailable")?
        .join("butterpollo.exe");
    let assets = executable.parent().unwrap().join("assets/web");
    if !executable.is_file() || !assets.is_dir() {
        bail!("Extract the whole Butterpollo package before opening Start Butterpollo.exe.");
    }
    let mut child = Command::new(&executable)
        .arg("--config-dir")
        .arg(&directory)
        .arg("--assets")
        .arg(assets)
        .current_dir(executable.parent().unwrap())
        .creation_flags(0x08000000)
        .spawn()
        .context("starting Butterpollo")?;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(response) = status(ports.http)
            && response.contains(&marker)
        {
            return crate::tray::open_web(ports.web);
        }
        if let Some(code) = child.try_wait()? {
            bail!(
                "Butterpollo could not start ({code}). Check {} for the startup error.",
                config
                    .path("log_path", &directory, "logs/butterpollo.log")
                    .display()
            );
        }
        if Instant::now() >= deadline {
            bail!(
                "Butterpollo is still starting. Open https://localhost:{} after startup, or check {}.",
                ports.web,
                directory.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
}
