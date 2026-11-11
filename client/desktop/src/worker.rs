//! Hintergrund-Thread: synchronisiert regelmäßig und sobald sich im
//! Sync-Ordner etwas ändert.

use std::{
    collections::VecDeque,
    sync::{
        mpsc::{self, RecvTimeoutError, Sender},
        Arc, Mutex,
    },
    thread,
    time::{Duration, Instant},
};

use dropcraft_core::{Api, ApiError, Event, State, SyncEngine, SyncError};
use notify::{RecursiveMode, Watcher};

use crate::config::{self, Config};

pub enum Cmd {
    SyncNow,
    SetPaused(bool),
    FsChanged,
    Stop,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Phase {
    Starting,
    Syncing,
    UpToDate,
    Paused,
    Offline(String),
    Error(String),
    NeedsLogin,
}

pub struct Shared {
    pub phase: Phase,
    pub activity: VecDeque<Event>,
    pub used: i64,
    pub quota: i64,
    pub last_sync: Option<chrono::DateTime<chrono::Local>>,
}

impl Default for Shared {
    fn default() -> Self {
        Self { phase: Phase::Starting, activity: VecDeque::new(), used: 0, quota: 0, last_sync: None }
    }
}

pub type SharedRef = Arc<Mutex<Shared>>;

const POLL: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(2500);
const MAX_ACTIVITY: usize = 100;

pub fn spawn(cfg: Config, shared: SharedRef, repaint: egui::Context) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let tx_watch = tx.clone();
    thread::Builder::new()
        .name("sync".into())
        .spawn(move || {
            let set = |p: Phase| {
                shared.lock().unwrap().phase = p;
                repaint.request_repaint();
            };

            let token = cfg.token.clone();
            let api = match Api::new(&cfg.server, token) {
                Ok(a) => a,
                Err(e) => return set(Phase::Error(e.to_string())),
            };
            let state = match State::open(&config::state_db_path()) {
                Ok(s) => s,
                Err(e) => return set(Phase::Error(format!("Datenbank: {e}"))),
            };
            // Gehört der gespeicherte Stand zu einem anderen Server/Konto/Ordner?
            // Dann von vorne anfangen, sonst würden Dateien falsch gelöscht.
            let binding = format!("{}|{}|{}", cfg.server, cfg.username, cfg.sync_dir.display());
            if state.meta("binding").as_deref() != Some(binding.as_str()) {
                let _ = state.clear();
                let _ = state.set_meta("binding", &binding);
            }
            let _ = std::fs::create_dir_all(&cfg.sync_dir);
            let mut engine = SyncEngine::new(api.clone(), cfg.sync_dir.clone(), state, cfg.device_name.clone());

            // Dateiänderungen beobachten.
            let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(ev) = res {
                    let relevant = ev.paths.iter().any(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .map(|n| !dropcraft_core::sync::is_ignored(n))
                            .unwrap_or(true)
                    });
                    if relevant {
                        let _ = tx_watch.send(Cmd::FsChanged);
                    }
                }
            })
            .ok();
            if let Some(w) = watcher.as_mut() {
                let _ = w.watch(&cfg.sync_dir, RecursiveMode::Recursive);
            }

            let mut paused = cfg.paused;
            loop {
                if paused {
                    set(Phase::Paused);
                    match rx.recv() {
                        Ok(Cmd::SetPaused(p)) => paused = p,
                        Ok(Cmd::Stop) | Err(_) => break,
                        Ok(_) => {}
                    }
                    continue;
                }

                set(Phase::Syncing);
                let result = engine.sync_once();
                let wait = {
                    let mut sh = shared.lock().unwrap();
                    let wait = match result {
                        Ok(report) => {
                            for ev in report.events {
                                sh.activity.push_front(ev);
                            }
                            sh.activity.truncate(MAX_ACTIVITY);
                            sh.last_sync = Some(chrono::Local::now());
                            sh.phase = Phase::UpToDate;
                            POLL
                        }
                        Err(SyncError::Api(ApiError::Unauthorized)) => {
                            sh.phase = Phase::NeedsLogin;
                            drop(sh);
                            repaint.request_repaint();
                            // Auf Abmelden/Neuanmeldung durch die Oberfläche warten.
                            loop {
                                match rx.recv() {
                                    Ok(Cmd::Stop) | Err(_) => return,
                                    _ => {}
                                }
                            }
                        }
                        Err(SyncError::Api(ApiError::Network(m))) => {
                            sh.phase = Phase::Offline(m);
                            RETRY
                        }
                        Err(e) => {
                            sh.phase = Phase::Error(e.to_string());
                            RETRY
                        }
                    };
                    wait
                };
                if let Ok(me) = api.me() {
                    let mut sh = shared.lock().unwrap();
                    sh.used = me.used_bytes;
                    sh.quota = me.quota_bytes;
                }
                repaint.request_repaint();

                // Warten auf Timer, Dateiänderung oder Befehl.
                match rx.recv_timeout(wait) {
                    Ok(Cmd::FsChanged) => {
                        // Weitere Änderungen sammeln, bis es kurz ruhig ist.
                        let start = Instant::now();
                        while start.elapsed() < Duration::from_secs(30) {
                            match rx.recv_timeout(DEBOUNCE) {
                                Ok(Cmd::FsChanged) => continue,
                                Ok(Cmd::Stop) => return,
                                Ok(Cmd::SetPaused(p)) => {
                                    paused = p;
                                    break;
                                }
                                Ok(Cmd::SyncNow) => break,
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    }
                    Ok(Cmd::SyncNow) | Err(RecvTimeoutError::Timeout) => {}
                    Ok(Cmd::SetPaused(p)) => paused = p,
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            drop(watcher);
        })
        .expect("Sync-Thread konnte nicht starten");
    tx
}

/// Vergisst den lokalen Sync-Stand (nach dem Abmelden).
pub fn reset_state() {
    if let Ok(s) = State::open(&config::state_db_path()) {
        let _ = s.clear();
    }
}
