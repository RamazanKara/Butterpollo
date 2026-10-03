//! Host housekeeping and user actions, independent of listener setup.
use crate::{maintenance, state::Shared};
use butterpollo_core::session::Role;
use butterpollo_windows::{process::StopSignal, tray::Action};
use std::{
    sync::{atomic::Ordering, mpsc::Receiver},
    time::{Duration, Instant},
};

fn application_finished(h: &Shared) -> bool {
    let connected = h
        .sessions
        .lock()
        .unwrap()
        .active
        .values()
        .any(|session| session.launch.role == Role::Stream && !session.stopping());
    h.current_app.lock().unwrap().as_mut().is_some_and(|app| {
        app.connection_state(connected)
            || match app.exited() {
                Ok(finished) => finished,
                Err(error) => {
                    tracing::warn!(%error, "application exit check failed");
                    false
                }
            }
    })
}
/// Start an app that was launched before anyone signed in, keeping its
/// session identity. It replaces the placeholder only if that still runs.
fn start_deferred(h: &Shared) {
    let pending = h.current_app.lock().unwrap().as_mut().and_then(|app| {
        app.take_ready_deferred()
            .map(|(app_config, args)| (app_config, args, app.generation.clone(), app.owner.clone()))
    });
    let Some((app, args, generation, owner)) = pending else {
        return;
    };
    let h = h.clone();
    tokio::task::spawn_blocking(move || {
        tracing::info!(app = %app.name, "user signed in; starting the deferred application");
        match crate::process::launch(&h, &app, &args) {
            Ok(mut started) => {
                let mut current = h.current_app.lock().unwrap();
                if current
                    .as_ref()
                    .is_some_and(|placeholder| placeholder.generation == generation)
                {
                    started.generation = generation;
                    started.owner = owner;
                    *current = Some(started);
                }
            }
            Err(error) => {
                tracing::warn!(%error, app = %app.name, "deferred application failed to start");
                h.sessions.lock().unwrap().stop_role(Role::Stream, None);
                h.stop_app();
            }
        }
    });
}
fn user_action(h: &Shared, action: Action, web_port: u16) {
    match action {
        Action::Open => {
            if let Err(error) = butterpollo_windows::tray::open_web(web_port) {
                tracing::warn!(%error, "could not open the administration console");
            }
        }
        Action::StopSessions => h.sessions.lock().unwrap().request_stop(None),
        Action::Restart => {
            h.restart.store(true, Ordering::Release);
            h.stop.store(true, Ordering::Release);
        }
        Action::Quit => h.stop.store(true, Ordering::Release),
    }
}
pub async fn maintain(
    h: Shared,
    stop_signal: StopSignal,
    actions: Option<Receiver<Action>>,
    web_port: u16,
) {
    let mut update_at = Instant::now();
    while !h.stop.load(Ordering::Acquire) {
        if stop_signal.requested() {
            tracing::info!("service requested host shutdown");
            h.stop.store(true, Ordering::Release);
            break;
        }
        h.sessions.lock().unwrap().expire();
        h.reap_paused_display();
        if Instant::now() >= update_at {
            let interval = h
                .config
                .read()
                .unwrap()
                .integer("update_check_interval", 86400);
            if interval > 0 {
                maintenance::trigger_update(&h);
            }
            update_at = Instant::now()
                + Duration::from_secs(if interval > 0 { interval as u64 } else { 60 });
        }
        start_deferred(&h);
        if application_finished(&h) {
            h.sessions.lock().unwrap().stop_role(Role::Stream, None);
            h.stop_app();
        }
        if let Some(actions) = &actions {
            while let Ok(action) = actions.try_recv() {
                user_action(&h, action, web_port);
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}
