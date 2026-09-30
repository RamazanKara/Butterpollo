use crate::{rtsp::Negotiated, state::Client};
use anyhow::{Result, bail};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    net::IpAddr,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
#[derive(Clone, Copy, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    Stream,
    RemoteMonitor,
    InputOnly,
}

#[cfg(test)]
mod tests {
    use super::*;
    fn launch(id: &str, role: Role) -> Launch {
        Launch {
            id: id.into(),
            client: Client {
                name: "fixture".into(),
                cert: String::new(),
                uuid: "client".into(),
                perm: u32::MAX,
                enabled: true,
                extra: Default::default(),
            },
            peer: "127.0.0.1".parse().unwrap(),
            app_id: 1,
            key: [0; 16],
            key_id: 1,
            ping: "ping".into(),
            connect_data: 1,
            role,
            created: Instant::now(),
            rtsp_encrypted: true,
            rtsp_counter: Arc::new(AtomicU32::new(1)),
            rtsp_received: Default::default(),
        }
    }
    #[test]
    fn independent_roles_and_targeted_teardown_preserve_other_sessions() {
        let mut sessions = Sessions::default();
        sessions.queue(launch("game", Role::Stream)).unwrap();
        sessions
            .queue(launch("monitor", Role::RemoteMonitor))
            .unwrap();
        sessions.queue(launch("input", Role::InputOnly)).unwrap();
        assert!(
            sessions
                .queue(launch("duplicate", Role::RemoteMonitor))
                .is_err()
        );
        let game = sessions
            .start(launch("game", Role::Stream), Negotiated::default())
            .unwrap();
        let input = sessions
            .start(launch("input", Role::InputOnly), Negotiated::default())
            .unwrap();
        sessions.stop_role(Role::RemoteMonitor, Some("client"));
        assert!(sessions.pending.is_empty());
        assert!(!game.stopping());
        assert!(!input.stopping());
        sessions.stop_role(Role::Stream, None);
        assert!(game.stopping());
        assert!(!input.stopping());
        sessions.request_stop(Some("client"));
        assert!(input.stopping());
    }
}
#[derive(Clone)]
pub struct Launch {
    pub id: String,
    pub client: Client,
    pub peer: IpAddr,
    pub app_id: u32,
    pub key: [u8; 16],
    pub key_id: u32,
    pub ping: String,
    pub connect_data: u32,
    pub role: Role,
    pub created: Instant,
    pub rtsp_encrypted: bool,
    pub rtsp_counter: Arc<AtomicU32>,
    pub rtsp_received: Arc<std::sync::Mutex<crate::packet::ReplayWindow>>,
}
#[derive(Default)]
pub struct Stats {
    pub frames: AtomicU64,
    pub packets: AtomicU64,
    pub bytes: AtomicU64,
    pub idr_requests: AtomicU64,
    pub latency_us: AtomicU64,
}
pub struct Session {
    pub launch: Launch,
    pub config: Negotiated,
    pub stop: AtomicBool,
    pub idr: AtomicBool,
    pub bitrate: AtomicU32,
    pub stats: Stats,
    pub started: Instant,
    pub output: std::sync::RwLock<String>,
}
impl Session {
    pub fn new(launch: Launch, config: Negotiated) -> Arc<Self> {
        let bitrate = config.bitrate_kbps;
        Arc::new(Self {
            launch,
            config,
            stop: AtomicBool::new(false),
            idr: AtomicBool::new(true),
            bitrate: AtomicU32::new(bitrate),
            stats: Stats::default(),
            started: Instant::now(),
            output: std::sync::RwLock::new(String::new()),
        })
    }
    pub fn stopping(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    pub fn stop(&self) {
        self.stop.store(true, Ordering::Release)
    }
    pub fn request_idr(&self) {
        self.idr.store(true, Ordering::Release);
        self.stats.idr_requests.fetch_add(1, Ordering::Relaxed);
    }
    pub fn info(&self) -> serde_json::Value {
        serde_json::json!({"uuid":self.launch.client.uuid,"device_name":self.launch.client.name,"width":self.config.width,"height":self.config.height,"fps":self.config.fps,"video_format":self.config.codec,"hdr":self.config.hdr,"encoder_bitrate_kbps":self.bitrate.load(Ordering::Relaxed),"audio_channels":self.config.audio_channels,"state":if self.stopping(){"STOPPING"}else{"RUNNING"},"frames_sent":self.stats.frames.load(Ordering::Relaxed),"packets_sent":self.stats.packets.load(Ordering::Relaxed),"bytes_sent":self.stats.bytes.load(Ordering::Relaxed),"idr_requests":self.stats.idr_requests.load(Ordering::Relaxed),"encode_latency_ms":self.stats.latency_us.load(Ordering::Relaxed) as f64/1000.,"uptime_seconds":self.started.elapsed().as_secs_f64(),"role":self.launch.role})
    }
}
#[derive(Default)]
pub struct Sessions {
    pub pending: BTreeMap<String, Launch>,
    pub active: BTreeMap<String, Arc<Session>>,
    pub teardown: usize,
}
impl Sessions {
    pub fn expire(&mut self) {
        self.pending
            .retain(|_, p| p.created.elapsed() < Duration::from_secs(30));
    }
    pub fn queue(&mut self, launch: Launch) -> Result<()> {
        self.expire();
        if self.pending.len() + self.active.len() >= 16 {
            bail!("session limit reached");
        }
        if self
            .pending
            .values()
            .any(|p| p.client.uuid == launch.client.uuid && p.role == launch.role)
            || self
                .active
                .values()
                .any(|p| p.launch.client.uuid == launch.client.uuid && p.launch.role == launch.role)
        {
            bail!("client already has a session");
        }
        self.pending.insert(launch.id.clone(), launch);
        Ok(())
    }
    pub fn pending_for_peer(&mut self, peer: IpAddr) -> Result<Launch> {
        self.expire();
        let mut found = self.pending.values().filter(|p| p.peer == peer);
        let p = found.next().cloned();
        if found.next().is_some() {
            bail!("ambiguous pending session identity");
        }
        p.ok_or_else(|| anyhow::anyhow!("no authorized launch for RTSP peer"))
    }
    pub fn rtsp_for_peer(&mut self, peer: IpAddr) -> Vec<Launch> {
        self.expire();
        self.pending
            .values()
            .chain(
                self.active
                    .values()
                    .filter(|s| !s.stopping())
                    .map(|s| &s.launch),
            )
            .filter(|p| p.peer == peer)
            .cloned()
            .collect()
    }
    pub fn start(&mut self, launch: Launch, config: Negotiated) -> Result<Arc<Session>> {
        if self.pending.remove(&launch.id).is_none() {
            bail!("launch expired or already consumed");
        }
        let s = Session::new(launch, config);
        self.active.insert(s.launch.id.clone(), s.clone());
        Ok(s)
    }
    pub fn request_stop(&mut self, id: Option<&str>) {
        self.pending
            .retain(|_, p| id.is_some_and(|id| p.client.uuid != id && p.id != id));
        for s in self.active.values() {
            if id.is_none_or(|id| s.launch.client.uuid == id || s.launch.id == id) {
                s.stop();
            }
        }
    }
    pub fn owns_capture(&self) -> bool {
        !self.pending.is_empty() || !self.active.is_empty() || self.teardown != 0
    }
    pub fn stop_role(&mut self, role: Role, client: Option<&str>) {
        self.pending
            .retain(|_, p| p.role != role || client.is_some_and(|id| p.client.uuid != id));
        for s in self.active.values() {
            if s.launch.role == role && client.is_none_or(|id| id == s.launch.client.uuid) {
                s.stop();
            }
        }
    }
}
