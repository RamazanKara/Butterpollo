//! Driver profile I/O stays on a disposable background thread, never encode.
use crate::nvapi::{Drs, Scope};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex, mpsc},
    time::{Duration, Instant},
};
type Tuning = BTreeMap<String, i64>;
fn values(drs: &Drs) -> Tuning {
    [
        ("rtx_hdr_peak_brightness", 0x00dd48fc, 400, 2000, 0),
        ("rtx_hdr_middle_gray", 0x00dd48fd, 10, 100, 0),
        ("rtx_hdr_contrast", 0x00dd48fe, 0, 200, -100),
        ("rtx_hdr_saturation", 0x00dd48ff, 0, 200, -100),
    ]
    .into_iter()
    .filter_map(|(key, id, min, max, offset)| {
        let raw = drs.profile_dword(id).ok().flatten()?;
        (min..=max)
            .contains(&raw)
            .then(|| (key.into(), i64::from(raw) + offset))
    })
    .collect()
}
fn lookup(executable: &str) -> Option<Tuning> {
    let global = Drs::open_scope(&Scope::Base, false).ok()?;
    let mut tuning = values(&global);
    drop(global);
    for path in [
        executable.to_owned(),
        executable.replace('\\', "/"),
        std::path::Path::new(executable)
            .file_name()?
            .to_string_lossy()
            .into_owned(),
    ] {
        if let Ok(app) = Drs::open_scope(&Scope::Application(path), false) {
            tuning.extend(values(&app));
            break;
        }
    }
    Some(tuning)
}
struct Reply {
    generation: u64,
    key: String,
    tuning: Option<Tuning>,
    elapsed: Duration,
}
pub struct Profiles {
    requests: mpsc::SyncSender<(u64, String)>,
    reply: Arc<Mutex<Option<Reply>>>,
    key: String,
    generation: u64,
    pending: bool,
    next: Instant,
    failures: u32,
    tuning: Tuning,
}
impl Profiles {
    pub fn new() -> Self {
        let (requests, receiver) = mpsc::sync_channel::<(u64, String)>(1);
        let reply = Arc::new(Mutex::new(None));
        let output = reply.clone();
        std::thread::spawn(move || {
            while let Ok((generation, key)) = receiver.recv() {
                let start = Instant::now();
                let tuning = lookup(&key);
                *output.lock().unwrap() = Some(Reply {
                    generation,
                    key,
                    tuning,
                    elapsed: start.elapsed(),
                });
            }
        });
        Self {
            requests,
            reply,
            key: String::new(),
            generation: 0,
            pending: false,
            next: Instant::now(),
            failures: 0,
            tuning: Tuning::new(),
        }
    }
    pub fn poll(&mut self, key: Option<&str>) -> &Tuning {
        let key = key.unwrap_or("");
        if key != self.key {
            self.key = key.into();
            self.generation = self.generation.wrapping_add(1);
            self.tuning.clear();
            self.pending = false;
            self.failures = 0;
            self.next = Instant::now();
        }
        if let Some(reply) = self.reply.lock().unwrap().take()
            && reply.generation == self.generation
            && reply.key == self.key
        {
            self.pending = false;
            if reply.tuning.is_none() || reply.elapsed > Duration::from_millis(100) {
                self.failures += 1;
            } else {
                self.failures = 0;
            }
            if let Some(tuning) = reply.tuning {
                self.tuning = tuning;
            }
            self.next = Instant::now()
                + Duration::from_secs(match self.failures {
                    0 => 5,
                    1 => 15,
                    _ => 30,
                });
        }
        if !key.is_empty()
            && !self.pending
            && Instant::now() >= self.next
            && self
                .requests
                .try_send((self.generation, key.into()))
                .is_ok()
        {
            self.pending = true;
        }
        &self.tuning
    }
}
impl Default for Profiles {
    fn default() -> Self {
        Self::new()
    }
}
