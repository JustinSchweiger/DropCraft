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

use dropcraft_core::{Api, ApiError, BandwidthRef, Event, PauseFlag, ProgressRef, State, SyncEngine, SyncError};
use notify::{RecursiveMode, Watcher};

use crate::{config::Account, folder_icon};

pub enum Cmd {
    SyncNow,
    #[allow(dead_code)]
    SetPaused(bool),
    FsChanged,
    Stop,
    /// Konto wird entfernt: Sync-Ordner bei Windows abmelden, dann beenden.
    Remove,
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
    pub progress: Option<ProgressRef>,
    pub is_admin: bool,
}

impl Default for Shared {
    fn default() -> Self {
        Self { phase: Phase::Starting, activity: VecDeque::new(), used: 0, quota: 0, last_sync: None, progress: None, is_admin: false }
    }
}

pub type SharedRef = Arc<Mutex<Shared>>;

const POLL: Duration = Duration::from_secs(30);
const RETRY: Duration = Duration::from_secs(60);
const DEBOUNCE: Duration = Duration::from_millis(2500);
const MAX_ACTIVITY: usize = 100;

pub fn spawn(cfg: Account, shared: SharedRef, repaint: egui::Context, pause_flag: PauseFlag, bandwidth: BandwidthRef) -> Sender<Cmd> {
    let (tx, rx) = mpsc::channel::<Cmd>();
    let tx_watch = tx.clone();
    #[cfg_attr(not(all(windows, feature = "cloud-files")), allow(unused_variables))]
    let tx_self = tx.clone();
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
            let _ = std::fs::create_dir_all(cfg.state_dir());
            let state = match State::open(&cfg.state_db()) {
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
            folder_icon::apply(&cfg.sync_dir);
            let mut engine = SyncEngine::new(api.clone(), cfg.sync_dir.clone(), state, cfg.device_name.clone());
            shared.lock().unwrap().progress = Some(engine.progress());
            engine.set_pause_flag(pause_flag.clone());
            engine.set_bandwidth(bandwidth);

            // Platzhalter statt vollständiger Downloads (Windows, optional).
            #[cfg(all(windows, feature = "cloud-files"))]
            let _cloud = match crate::cloudfiles::connect(&cfg.id, &cfg.username, &cfg.sync_dir, api.clone(), engine.progress(), tx_self.clone()) {
                Ok((hooks, root)) => {
                    engine.set_hooks(Box::new(hooks));
                    Some(root)
                }
                Err(e) => {
                    shared.lock().unwrap().activity.push_front(Event {
                        kind: dropcraft_core::EventKind::Error,
                        path: "Cloud-Dateien".into(),
                        detail: e,
                        at: chrono::Local::now(),
                    });
                    None
                }
            };

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

            let is_paused = || pause_flag.load(std::sync::atomic::Ordering::SeqCst);
            loop {
                if is_paused() {
                    set(Phase::Paused);
                    match rx.recv() {
                        // Der Zustand steht im gemeinsamen Schalter; die Nachricht weckt nur auf.
                        Ok(Cmd::SetPaused(_)) => {}
                        Ok(Cmd::Remove) => {
                            remove_cloud(&cfg);
                            break;
                        }
                        Ok(Cmd::Stop) | Err(_) => break,
                        Ok(_) => {}
                    }
                    continue;
                }

                set(Phase::Syncing);
                let result = engine.sync_once();
                let wait = {
                    let mut sh = shared.lock().unwrap();
                    if result.is_err() {
                        // Ereignisse eines abgebrochenen Durchgangs nicht verlieren.
                        if let Some(p) = sh.progress.clone() {
                            let evs = std::mem::take(&mut p.lock().unwrap().events);
                            for ev in evs {
                                sh.activity.push_front(ev);
                            }
                        }
                    }
                    let wait = match result {
                        Ok(report) => {
                            if !report.events.is_empty() {
                                let (api, dev, evs) = (api.clone(), cfg.device_name.clone(), report.events.clone());
                                std::thread::spawn(move || crate::notify::shared_folder_changes(&api, &dev, &evs));
                            }
                            for ev in report.events {
                                sh.activity.push_front(ev);
                            }
                            if let Some(p) = &sh.progress {
                                p.lock().unwrap().events.clear();
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
                                    Ok(Cmd::Remove) => {
                                        remove_cloud(&cfg);
                                        return;
                                    }
                                    Ok(Cmd::Stop) | Err(_) => return,
                                    _ => {}
                                }
                            }
                        }
                        Err(SyncError::Paused) => {
                            sh.phase = Phase::Paused;
                            Duration::from_millis(10)
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
                    sh.is_admin = me.is_admin;
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
                                Ok(Cmd::Remove) => {
                                    remove_cloud(&cfg);
                                    return;
                                }
                                Ok(Cmd::SetPaused(_)) => break,
                                Ok(Cmd::SyncNow) => break,
                                Err(RecvTimeoutError::Timeout) => break,
                                Err(RecvTimeoutError::Disconnected) => return,
                            }
                        }
                    }
                    Ok(Cmd::SyncNow) | Err(RecvTimeoutError::Timeout) => {}
                    Ok(Cmd::SetPaused(_)) => {}
                    Ok(Cmd::Remove) => {
                        remove_cloud(&cfg);
                        break;
                    }
                    Ok(Cmd::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                }
            }
            drop(watcher);
        })
        .expect("Sync-Thread konnte nicht starten");
    tx
}

/// Beim Entfernen eines Kontos: alle Platzhalter herunterladen und den
/// Sync-Ordner bei Windows abmelden, damit keine Dateien verschwinden.
fn remove_cloud(_cfg: &Account) {
    #[cfg(all(windows, feature = "cloud-files"))]
    crate::cloudfiles::unregister(&_cfg.id, &_cfg.sync_dir);
}
