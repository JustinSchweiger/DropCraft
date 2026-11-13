//! Die Sync-Engine: gleicht einen lokalen Ordner mit dem Server ab.
//!
//! Ablauf eines Durchgangs (`sync_once`):
//! 1. **Pull** – Änderungen vom Server seit dem letzten Cursor holen und lokal
//!    anwenden (herunterladen, Ordner anlegen, löschen).
//! 2. **Push** – lokalen Ordner scannen und mit dem gespeicherten Stand
//!    vergleichen: neue/geänderte Dateien hochladen, gelöschte entfernen.
//!
//! Wurde eine Datei auf beiden Seiten geändert, bleibt nichts verloren: Die
//! lokale Version wird als „… (Konflikt <Gerät> <Datum>)“ umbenannt und
//! ebenfalls hochgeladen, die Server-Version landet unter dem Originalnamen.

use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    fs,
    io::Read,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};

use sha2::{Digest, Sha256};

use crate::{
    api::{Api, ApiError, RemoteEntry},
    bandwidth::{BandwidthRef, PauseFlag},
    hooks::{FullDownload, LocalHooks},
    progress::{self, ProgressRef},
    state::{State, Synced},
};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("Datenbankfehler: {0}")]
    Db(#[from] rusqlite::Error),
    #[error("Dateifehler: {0}")]
    Io(#[from] std::io::Error),
    #[error("Sync-Ordner nicht gefunden: {0}")]
    RootMissing(String),
    #[error("Pausiert")]
    Paused,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventKind {
    Uploaded,
    Downloaded,
    /// Neu vom Server, nur als Platzhalter (nicht heruntergeladen).
    NewOnline,
    DeletedLocal,
    DeletedRemote,
    Conflict,
    Error,
}

#[derive(Debug, Clone)]
pub struct Event {
    pub kind: EventKind,
    pub path: String,
    pub detail: String,
    pub at: chrono::DateTime<chrono::Local>,
    /// Wer die Änderung gemacht hat (bei Änderungen vom Server).
    pub by: Option<String>,
}

#[derive(Debug, Default)]
pub struct Report {
    pub uploaded: usize,
    pub downloaded: usize,
    pub deleted: usize,
    pub conflicts: usize,
    pub errors: usize,
    pub events: Vec<Event>,
}

impl Report {
    pub fn changed(&self) -> bool {
        self.uploaded + self.downloaded + self.deleted + self.conflicts > 0
    }
}

/// Dateien, die nie synchronisiert werden.
pub fn is_ignored(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    name.starts_with(".dropcraft-")
        || name.starts_with("~$")
        || lower.ends_with(".tmp")
        || lower.ends_with(".crdownload")
        || lower.ends_with(".part")
        || lower == "desktop.ini"
        || lower == "thumbs.db"
        || lower == ".ds_store"
}

/// Dateien, die gerade erst geändert wurden, warten bis sie „ruhig“ sind.
const SETTLE_MS: i64 = 2000;

/// So viele Dateien werden gleichzeitig hoch- bzw. heruntergeladen.
pub const PARALLEL: usize = 4;

/// Arbeitet `jobs` mit bis zu `n` Threads ab. `done` läuft auf dem
/// aufrufenden Thread, sobald ein Ergebnis da ist; gibt es `true` zurück
/// (schwerer Fehler), werden keine neuen Aufträge mehr begonnen.
fn parallel<J: Send, R: Send>(jobs: Vec<J>, n: usize, work: impl Fn(&J) -> R + Sync, mut done: impl FnMut(J, R) -> bool) {
    if jobs.is_empty() {
        return;
    }
    let threads = n.max(1).min(jobs.len());
    let queue = Mutex::new(jobs.into_iter().collect::<VecDeque<_>>());
    let stop = AtomicBool::new(false);
    let (tx, rx) = mpsc::channel::<(J, R)>();
    std::thread::scope(|scope| {
        for _ in 0..threads {
            let tx = tx.clone();
            let (queue, stop, work) = (&queue, &stop, &work);
            scope.spawn(move || loop {
                if stop.load(Ordering::SeqCst) {
                    break;
                }
                let Some(job) = queue.lock().unwrap().pop_front() else { break };
                let r = work(&job);
                if tx.send((job, r)).is_err() {
                    break;
                }
            });
        }
        drop(tx);
        for (job, r) in rx {
            if done(job, r) {
                stop.store(true, Ordering::SeqCst);
            }
        }
    });
}

/// Rückruf für eine Übertragung: Fortschritt, Bandbreite, Abbruch.
fn transfer_cb(prog: &ProgressRef, bw: &BandwidthRef, pause: &PauseFlag, id: u64, upload: bool) -> impl Fn(u64) -> std::io::Result<()> + Send + 'static {
    let (prog, bw, pause) = (prog.clone(), bw.clone(), pause.clone());
    move |n| {
        if pause.load(Ordering::SeqCst) {
            return Err(std::io::Error::new(std::io::ErrorKind::Interrupted, "Pausiert"));
        }
        if upload {
            bw.up.take(n);
        } else {
            bw.down.take(n);
        }
        progress::advance(&prog, id, n);
        Ok(())
    }
}

/// Lädt eine Datei vom Server (Teil, der parallel laufen darf).
fn fetch_file(api: &Api, prog: &ProgressRef, bw: &BandwidthRef, pause: &PauseFlag, e: &RemoteEntry, local: &Path) -> Result<(), SyncError> {
    let parent = local.parent().ok_or_else(|| std::io::Error::other("Ungültiger Pfad"))?.to_path_buf();
    fs::create_dir_all(&parent)?;
    let tmp = parent.join(format!(".dropcraft-{}.tmp", uuid::Uuid::new_v4()));
    let id = progress::start(prog, &e.path, false, e.size as u64);
    let res = (|| -> Result<(), SyncError> {
        let cb = transfer_cb(prog, bw, pause, id, false);
        api.download_with_progress(&e.path, &tmp, &mut |n| cb(n))?;
        if let Some(expected) = &e.sha256 {
            let got = hash_file(&tmp)?;
            if &got != expected {
                return Err(SyncError::Io(std::io::Error::other("Prüfsumme stimmt nicht")));
            }
        }
        let ft = filetime::FileTime::from_unix_time(e.modified_at / 1000, ((e.modified_at % 1000) * 1_000_000) as u32);
        let _ = filetime::set_file_mtime(&tmp, ft);
        fs::rename(&tmp, local)?;
        Ok(())
    })();
    progress::finish(prog, id, res.is_ok());
    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Ergebnis eines Upload-Auftrags.
enum Pushed {
    /// Nur die Änderungszeit hatte sich geändert (gleicher Inhalt).
    Touched,
    Uploaded(RemoteEntry),
}

struct UploadJob {
    path: String,
    local: PathBuf,
    item: LocalItem,
    st: Option<Synced>,
}

/// Lädt eine Datei hoch (Teil, der parallel laufen darf).
fn send_file(api: &Api, prog: &ProgressRef, bw: &BandwidthRef, pause: &PauseFlag, j: &UploadJob) -> Result<Pushed, SyncError> {
    // Schon beim Prüfen als laufende Datei anzeigen.
    let id = progress::start(prog, &j.path, true, j.item.size as u64);
    let res = (|| {
        let sha = hash_file(&j.local)?;
        if j.st.as_ref().and_then(|s| s.sha256.as_deref()) == Some(sha.as_str()) {
            return Ok(Pushed::Touched);
        }
        let base_rev = j.st.as_ref().map(|s| s.rev).unwrap_or(0);
        let r = api.upload_with_progress(&j.path, &j.local, j.item.mtime, base_rev, transfer_cb(prog, bw, pause, id, true))?;
        Ok(Pushed::Uploaded(r))
    })();
    progress::finish(prog, id, matches!(res, Ok(Pushed::Uploaded(_))));
    res
}

#[derive(Debug, Clone)]
struct LocalItem {
    is_dir: bool,
    size: i64,
    mtime: i64,
}

pub struct SyncEngine {
    api: Api,
    root: PathBuf,
    state: State,
    device: String,
    report: Report,
    progress: ProgressRef,
    hooks: Box<dyn LocalHooks>,
    bandwidth: BandwidthRef,
    pause: PauseFlag,
}

impl SyncEngine {
    pub fn new(api: Api, root: PathBuf, state: State, device: String) -> Self {
        Self {
            api,
            root,
            state,
            device,
            report: Report::default(),
            progress: ProgressRef::default(),
            hooks: Box::new(FullDownload),
            bandwidth: BandwidthRef::default(),
            pause: PauseFlag::default(),
        }
    }

    /// Gemeinsame Bandbreitenlimits (für alle Konten dieselben).
    pub fn set_bandwidth(&mut self, bw: BandwidthRef) {
        self.bandwidth = bw;
    }

    /// Pausieren-Schalter: bricht laufende Übertragungen sofort ab.
    pub fn set_pause_flag(&mut self, flag: PauseFlag) {
        self.pause = flag;
    }

    fn check_pause(&self) -> Result<(), SyncError> {
        if self.pause.load(std::sync::atomic::Ordering::SeqCst) {
            Err(SyncError::Paused)
        } else {
            Ok(())
        }
    }

    /// Platzhalter-Unterstützung einschalten (siehe [`LocalHooks`]).
    pub fn set_hooks(&mut self, hooks: Box<dyn LocalHooks>) {
        self.hooks = hooks;
    }

    /// Live-Fortschritt (für die Oberfläche).
    pub fn progress(&self) -> ProgressRef {
        self.progress.clone()
    }

    pub fn api(&self) -> &Api {
        &self.api
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Ein kompletter Abgleich. Netzwerk- und Anmeldefehler brechen ab,
    /// Fehler bei einzelnen Dateien werden als Ereignis gemeldet.
    pub fn sync_once(&mut self) -> Result<Report, SyncError> {
        self.report = Report::default();
        self.progress.lock().unwrap().reset_round();
        if !self.root.is_dir() {
            return Err(SyncError::RootMissing(self.root.display().to_string()));
        }
        // Bei Konflikten ist eine weitere Runde nötig (Kopie hochladen,
        // Server-Version holen). Mehr als drei Runden braucht es nie.
        for _ in 0..3 {
            let before = self.report.conflicts;
            self.pull()?;
            self.push()?;
            self.availability()?;
            if self.report.conflicts == before {
                break;
            }
        }
        {
            let mut p = self.progress.lock().unwrap();
            p.queue.clear();
            p.active.clear();
        }
        Ok(std::mem::take(&mut self.report))
    }

    // ------------------------------------------------------------ pull

    fn pull(&mut self) -> Result<(), SyncError> {
        loop {
            let cursor = self.state.cursor();
            let changes = self.api.changes(cursor)?;
            for e in &changes.entries {
                if !e.deleted && !e.is_dir && self.state.get(&e.path).map(|s| s.rev) != Some(e.rev) {
                    progress::enqueue(&self.progress, &e.path, false, e.size as u64);
                }
            }
            // Erst alles Schnelle (Ordner, Löschungen, Platzhalter) der Reihe
            // nach, die eigentlichen Downloads danach parallel.
            let mut downloads: Vec<(RemoteEntry, PathBuf)> = Vec::new();
            for e in &changes.entries {
                self.check_pause()?;
                let res = self.apply_remote(e);
                match res {
                    Ok(Some(local)) => {
                        downloads.push((e.clone(), local));
                        continue;
                    }
                    Ok(None) => {}
                    Err(SyncError::Paused) => return Err(SyncError::Paused),
                    Err(SyncError::Api(err @ (ApiError::Network(_) | ApiError::Unauthorized))) => {
                        return Err(err.into())
                    }
                    Err(err) => self.event(EventKind::Error, &e.path, err.to_string()),
                }
                progress::drop_queued(&self.progress, &e.path, false);
            }
            self.download_many(downloads, true)?;
            self.state.set_cursor(changes.cursor)?;
            if !changes.has_more {
                return Ok(());
            }
        }
    }

    /// Wendet eine Server-Änderung an. Muss die Datei heruntergeladen werden,
    /// kommt ihr lokaler Pfad zurück (der Download läuft danach parallel).
    fn apply_remote(&mut self, e: &RemoteEntry) -> Result<Option<PathBuf>, SyncError> {
        let local = self.local_path(&e.path);
        let st = self.state.get(&e.path);

        if e.deleted {
            if st.is_some() {
                match fs::symlink_metadata(&local) {
                    Ok(m) if m.is_dir() => {
                        // Nur leere Ordner löschen; neue lokale Dateien bleiben.
                        if fs::remove_dir(&local).is_ok() {
                            self.event_by(EventKind::DeletedLocal, &e.path, String::new(), e.modified_by.clone());
                        }
                    }
                    Ok(m) => {
                        let s = st.as_ref().unwrap();
                        if local_unchanged(s, &m) {
                            fs::remove_file(&local)?;
                            self.event_by(EventKind::DeletedLocal, &e.path, String::new(), e.modified_by.clone());
                        }
                        // Lokal geändert: Datei behalten, sie wird neu hochgeladen.
                    }
                    Err(_) => {}
                }
                self.state.remove_tree(&e.path)?;
            }
            return Ok(None);
        }

        if e.is_dir {
            if local.is_file() {
                self.move_to_conflict(&e.path)?;
            }
            fs::create_dir_all(&local)?;
            self.hooks.mark_synced(&local, true);
            self.state.put(&Synced {
                path: e.path.clone(),
                is_dir: true,
                rev: e.rev,
                sha256: None,
                size: 0,
                local_mtime: 0,
            })?;
            return Ok(None);
        }

        // Datei
        if let Some(s) = &st {
            if s.rev == e.rev && local.is_file() {
                return Ok(None); // schon aktuell (z. B. eigener Upload)
            }
        }
        match fs::symlink_metadata(&local) {
            Ok(m) if m.is_dir() => {
                return Err(SyncError::Io(std::io::Error::other(
                    "Lokal existiert ein Ordner mit diesem Namen",
                )));
            }
            Ok(m) => {
                // Platzhalter ohne Inhalt kann lokal niemand geändert haben.
                if self.hooks.is_online_only(&local) {
                    if !self.hooks.wants_local(&local) && self.hooks.update_placeholder(&local, e)? {
                        self.remember_file(e, &local)?;
                        return Ok(None);
                    }
                } else {
                    let unchanged = st.as_ref().map(|s| local_unchanged(s, &m)).unwrap_or(false);
                    if unchanged && !self.hooks.wants_local(&local) && self.hooks.update_placeholder(&local, e)? {
                        // Unveränderte Datei, die nicht angepinnt ist: nicht neu
                        // laden, sondern zum Platzhalter der neuen Version machen.
                        self.remember_file(e, &local)?;
                        return Ok(None);
                    }
                    if !unchanged {
                        let sha = hash_file(&local)?;
                        if Some(&sha) == e.sha256.as_ref() {
                            // Gleicher Inhalt auf beiden Seiten: nur merken.
                            self.remember_file(e, &local)?;
                            self.hooks.mark_synced(&local, false);
                            return Ok(None);
                        }
                        let local_is_last_synced = st.as_ref().and_then(|s| s.sha256.as_ref()) == Some(&sha);
                        if !local_is_last_synced {
                            // Beide Seiten geändert -> lokale Version sichern.
                            self.move_to_conflict(&e.path)?;
                        }
                    }
                }
            }
            Err(_) => {
                // Neu vom Server: als Platzhalter anlegen, wenn gewünscht.
                if !self.hooks.wants_local(&local) {
                    if let Some(parent) = local.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    if self.hooks.create_placeholder(&local, e)? {
                        self.remember_file(e, &local)?;
                        self.event_by(EventKind::NewOnline, &e.path, human_size(e.size), e.modified_by.clone());
                        return Ok(None);
                    }
                }
            }
        }
        Ok(Some(local))
    }

    /// Lädt mehrere Dateien parallel herunter und trägt sie danach ein.
    /// `announce`: „heruntergeladen“-Ereignis melden.
    fn download_many(&mut self, jobs: Vec<(RemoteEntry, PathBuf)>, announce: bool) -> Result<(), SyncError> {
        let (api, prog, bw, pause) = (self.api.clone(), self.progress.clone(), self.bandwidth.clone(), self.pause.clone());
        let mut fatal: Option<SyncError> = None;
        parallel(
            jobs,
            PARALLEL,
            |(e, local)| fetch_file(&api, &prog, &bw, &pause, e, local),
            |(e, local), res| {
                progress::drop_queued(&self.progress, &e.path, false);
                let res = res.and_then(|_| {
                    self.hooks.mark_synced(&local, false);
                    self.remember_file(&e, &local)
                });
                match res {
                    Ok(()) => {
                        if announce {
                            self.event_by(EventKind::Downloaded, &e.path, human_size(e.size), e.modified_by.clone());
                        }
                        false
                    }
                    Err(_) if self.pause.load(Ordering::SeqCst) => {
                        fatal = Some(SyncError::Paused);
                        true
                    }
                    Err(SyncError::Api(err @ (ApiError::Network(_) | ApiError::Unauthorized))) => {
                        fatal.get_or_insert(SyncError::Api(err));
                        true
                    }
                    Err(err) => {
                        self.event(EventKind::Error, &e.path, err.to_string());
                        false
                    }
                }
            },
        );
        match fatal {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    fn remember_file(&mut self, e: &RemoteEntry, local: &Path) -> Result<(), SyncError> {
        let m = fs::metadata(local)?;
        self.state.put(&Synced {
            path: e.path.clone(),
            is_dir: false,
            rev: e.rev,
            sha256: e.sha256.clone(),
            size: e.size,
            local_mtime: mtime_ms(&m),
        })?;
        Ok(())
    }

    // ------------------------------------------------------------ push

    fn push(&mut self) -> Result<(), SyncError> {
        let local = self.scan()?;
        let now = now_ms();

        // 1. Lokal gelöschte Einträge auf dem Server löschen.
        let mut deleted_prefixes: Vec<String> = Vec::new();
        for s in self.state.all()? {
            if local.contains_key(&s.path) {
                continue;
            }
            if deleted_prefixes.iter().any(|p| s.path.starts_with(&format!("{p}/"))) {
                continue;
            }
            match self.api.delete(&s.path) {
                Ok(()) | Err(ApiError::NotFound) => {
                    self.state.remove_tree(&s.path)?;
                    deleted_prefixes.push(s.path.clone());
                    self.report.deleted += 1;
                    self.event(EventKind::DeletedRemote, &s.path, String::new());
                }
                Err(e @ (ApiError::Network(_) | ApiError::Unauthorized)) => return Err(e.into()),
                Err(e) => self.event(EventKind::Error, &s.path, e.to_string()),
            }
        }

        // 2. Neue Ordner und neue/geänderte Dateien hochladen.
        //    Vorher prüfen, was noch in den Speicher passt.
        let me = self.api.me()?;
        let mut free = me.quota_bytes - me.used_bytes;
        for (path, item) in &local {
            if item.is_dir || now - item.mtime < SETTLE_MS || self.hooks.is_online_only(&self.local_path(path)) {
                continue;
            }
            let changed = match self.state.get(path) {
                Some(s) => s.size != item.size || s.local_mtime != item.mtime,
                None => true,
            };
            if changed {
                progress::enqueue(&self.progress, path, true, item.size as u64);
            }
        }
        let mut uploads: Vec<UploadJob> = Vec::new();
        for (path, item) in &local {
            self.check_pause()?;
            let st = self.state.get(path);
            let res: Result<(), SyncError> = if item.is_dir {
                if st.as_ref().map(|s| s.is_dir).unwrap_or(false) {
                    Ok(())
                } else {
                    self.api.mkdir(path).map_err(Into::into).and_then(|r| {
                        self.hooks.mark_synced(&self.local_path(path), true);
                        self.state
                            .put(&Synced { path: path.clone(), is_dir: true, rev: r.rev, sha256: None, size: 0, local_mtime: 0 })
                            .map_err(Into::into)
                    })
                }
            } else {
                if self.hooks.is_online_only(&self.local_path(path)) {
                    continue; // Platzhalter ohne Inhalt: nichts hochzuladen
                }
                if let Some(s) = &st {
                    if s.size == item.size && s.local_mtime == item.mtime {
                        continue; // unverändert
                    }
                }
                if now - item.mtime < SETTLE_MS {
                    continue; // wird vermutlich gerade noch geschrieben
                }
                let needed = item.size - st.as_ref().map(|s| s.size).unwrap_or(0);
                if needed > free {
                    progress::drop_queued(&self.progress, path, true);
                    Err(SyncError::Api(ApiError::QuotaExceeded))
                } else {
                    // Platz vormerken; hochgeladen wird gleich parallel.
                    free -= needed;
                    uploads.push(UploadJob { path: path.clone(), local: self.local_path(path), item: item.clone(), st });
                    continue;
                }
            };
            match res {
                Ok(()) => {}
                Err(SyncError::Paused) => return Err(SyncError::Paused),
                Err(SyncError::Api(e @ (ApiError::Network(_) | ApiError::Unauthorized))) => return Err(e.into()),
                Err(SyncError::Api(ApiError::QuotaExceeded)) => {
                    self.event(EventKind::Error, path, "Speicherplatz voll".into());
                }
                Err(e) => self.event(EventKind::Error, path, e.to_string()),
            }
        }
        self.upload_many(uploads)
    }

    /// Lädt mehrere Dateien parallel hoch und trägt die Ergebnisse ein.
    fn upload_many(&mut self, jobs: Vec<UploadJob>) -> Result<(), SyncError> {
        let (api, prog, bw, pause) = (self.api.clone(), self.progress.clone(), self.bandwidth.clone(), self.pause.clone());
        let mut fatal: Option<SyncError> = None;
        parallel(
            jobs,
            PARALLEL,
            |j| send_file(&api, &prog, &bw, &pause, j),
            |j, res| {
                progress::drop_queued(&self.progress, &j.path, true);
                let res = match res {
                    Ok(Pushed::Touched) => match &j.st {
                        Some(s) => self.state.put(&Synced { local_mtime: j.item.mtime, ..s.clone() }).map_err(Into::into),
                        None => Ok(()),
                    },
                    Ok(Pushed::Uploaded(r)) => {
                        // Den Stand vom Scan merken. Hat sich die Datei während des
                        // Uploads geändert, erkennt der nächste Scan das und lädt neu.
                        let put = self.state.put(&Synced {
                            path: r.path.clone(),
                            is_dir: false,
                            rev: r.rev,
                            sha256: r.sha256.clone(),
                            size: j.item.size,
                            local_mtime: j.item.mtime,
                        });
                        put.map_err(SyncError::from).map(|_| {
                            self.report.uploaded += 1;
                            self.hooks.mark_synced(&j.local, false);
                            self.event(EventKind::Uploaded, &j.path, human_size(r.size));
                        })
                    }
                    // Server hat eine neuere Version: lokale als Kopie sichern,
                    // die nächste Runde holt die Server-Version.
                    Err(SyncError::Api(ApiError::Conflict(_))) => self.move_to_conflict(&j.path),
                    Err(e) => Err(e),
                };
                match res {
                    Ok(()) => false,
                    Err(_) if self.pause.load(Ordering::SeqCst) => {
                        fatal = Some(SyncError::Paused);
                        true
                    }
                    Err(SyncError::Api(err @ (ApiError::Network(_) | ApiError::Unauthorized))) => {
                        fatal.get_or_insert(SyncError::Api(err));
                        true
                    }
                    Err(SyncError::Api(ApiError::QuotaExceeded)) => {
                        self.event(EventKind::Error, &j.path, "Speicherplatz voll".into());
                        false
                    }
                    Err(e) => {
                        self.event(EventKind::Error, &j.path, e.to_string());
                        false
                    }
                }
            },
        );
        match fatal {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    // ------------------------------------------------------------ Verfügbarkeit

    /// Angepinnte Platzhalter herunterladen, freigegebene Dateien leeren.
    fn availability(&mut self) -> Result<(), SyncError> {
        // Angepinnte Platzhalter: werden am Ende parallel gefüllt.
        let mut pinned: Vec<(Synced, PathBuf)> = Vec::new();
        for s in self.state.all()? {
            let local = self.local_path(&s.path);
            if s.is_dir {
                if local.is_dir() {
                    self.hooks.check_synced(&local, true);
                }
                continue;
            }
            if !local.is_file() {
                continue;
            }
            let online_only = self.hooks.is_online_only(&local);
            if online_only && self.hooks.wants_local(&local) {
                progress::enqueue(&self.progress, &s.path, false, s.size as u64);
                pinned.push((s, local));
            } else if online_only {
                self.hooks.check_synced(&local, false);
            } else if fs::metadata(&local).map(|m| local_unchanged(&s, &m)).unwrap_or(false) && !self.hooks.wants_free(&local) {
                self.hooks.check_synced(&local, false);
            } else if self.hooks.wants_free(&local) {
                let unchanged = fs::metadata(&local).map(|m| local_unchanged(&s, &m)).unwrap_or(false);
                if unchanged {
                    if let Err(err) = self.hooks.free_space(&local) {
                        // Datei gerade geöffnet (z. B. Video läuft): später erneut versuchen.
                        if err.raw_os_error() != Some(32) {
                            self.event(EventKind::Error, &s.path, format!("Speicher freigeben: {err}"));
                        }
                    } else if let Ok(m) = fs::metadata(&local) {
                        self.state.put(&Synced { local_mtime: mtime_ms(&m), ..s.clone() })?;
                    }
                }
            }
        }
        self.fill_pinned(pinned)
    }

    /// Angepinnte Platzhalter parallel füllen: an Ort und Stelle (Windows ruft
    /// dafür den Provider auf), sonst klassisch herunterladen.
    fn fill_pinned(&mut self, pinned: Vec<(Synced, PathBuf)>) -> Result<(), SyncError> {
        if pinned.is_empty() {
            return Ok(());
        }
        // Hooks kurz herausnehmen, damit die Threads sie gemeinsam nutzen können.
        let hooks = std::mem::replace(&mut self.hooks, Box::new(FullDownload));
        let mut fallback: Vec<(RemoteEntry, PathBuf)> = Vec::new();
        let mut fatal: Option<SyncError> = None;
        parallel(
            pinned,
            PARALLEL,
            |(_, local)| hooks.hydrate(local),
            |(s, local), res| match res {
                Ok(true) => {
                    progress::drop_queued(&self.progress, &s.path, false);
                    hooks.mark_synced(&local, false);
                    if let Ok(m) = fs::metadata(&local) {
                        let _ = self.state.put(&Synced { local_mtime: mtime_ms(&m), ..s.clone() });
                    }
                    self.event(EventKind::Downloaded, &s.path, human_size(s.size));
                    false
                }
                Ok(false) => {
                    let modified = fs::metadata(&local).ok().map(|m| mtime_ms(&m)).unwrap_or(0);
                    let e = RemoteEntry {
                        path: s.path.clone(),
                        is_dir: false,
                        size: s.size,
                        sha256: s.sha256.clone(),
                        modified_at: modified,
                        rev: s.rev,
                        deleted: false,
                        modified_by: None,
                    };
                    fallback.push((e, local));
                    false
                }
                Err(err) => {
                    progress::drop_queued(&self.progress, &s.path, false);
                    if self.pause.load(Ordering::SeqCst) {
                        fatal = Some(SyncError::Paused);
                        return true;
                    }
                    self.event(EventKind::Error, &s.path, format!("Herunterladen: {err}"));
                    false
                }
            },
        );
        self.hooks = hooks;
        if let Some(e) = fatal {
            return Err(e);
        }
        self.download_many(fallback, true)
    }

    // ------------------------------------------------------------ helpers

    fn scan(&self) -> Result<BTreeMap<String, LocalItem>, SyncError> {
        let mut out = BTreeMap::new();
        let walker = walkdir::WalkDir::new(&self.root)
            .min_depth(1)
            .follow_links(false)
            .into_iter()
            .filter_entry(|d| d.file_name().to_str().map(|n| !is_ignored(n)).unwrap_or(false));
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue, // z. B. keine Leserechte
            };
            let ft = entry.file_type();
            if ft.is_symlink() {
                continue;
            }
            let rel = match entry.path().strip_prefix(&self.root) {
                Ok(r) => r,
                Err(_) => continue,
            };
            let Some(path) = to_remote(rel) else { continue };
            let m = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            out.insert(
                path,
                LocalItem { is_dir: ft.is_dir(), size: if ft.is_dir() { 0 } else { m.len() as i64 }, mtime: mtime_ms(&m) },
            );
        }
        Ok(out)
    }

    fn local_path(&self, remote: &str) -> PathBuf {
        let mut p = self.root.clone();
        for part in remote.split('/') {
            p.push(part);
        }
        p
    }

    /// Benennt die lokale Datei in eine Konflikt-Kopie um.
    fn move_to_conflict(&mut self, path: &str) -> Result<(), SyncError> {
        let local = self.local_path(path);
        let stamp = chrono::Local::now().format("%Y-%m-%d %H-%M");
        let (stem, ext) = split_ext(local.file_name().and_then(|n| n.to_str()).unwrap_or("Datei"));
        let parent = local.parent().unwrap_or(&self.root).to_path_buf();
        let mut target = parent.join(format!("{stem} (Konflikt {} {stamp}){ext}", self.device));
        let mut n = 2;
        while target.exists() {
            target = parent.join(format!("{stem} (Konflikt {} {stamp} {n}){ext}", self.device));
            n += 1;
        }
        fs::rename(&local, &target)?;
        self.report.conflicts += 1;
        let name = target.file_name().and_then(|n| n.to_str()).unwrap_or("").to_string();
        self.event(EventKind::Conflict, path, format!("Deine Version liegt jetzt unter „{name}“"));
        Ok(())
    }

    fn event(&mut self, kind: EventKind, path: &str, detail: String) {
        self.event_by(kind, path, detail, None);
    }

    fn event_by(&mut self, kind: EventKind, path: &str, detail: String, by: Option<String>) {
        match kind {
            EventKind::Downloaded => self.report.downloaded += 1,
            EventKind::DeletedLocal => self.report.deleted += 1,
            EventKind::Error => self.report.errors += 1,
            _ => {}
        }
        log::info!("{kind:?} {path} {detail}");
        let ev = Event { kind, path: path.to_string(), detail, at: chrono::Local::now(), by };
        self.progress.lock().unwrap().events.push(ev.clone());
        self.report.events.push(ev);
    }

    /// Vergisst den lokalen Stand (z. B. nach dem Abmelden).
    pub fn reset(&self) -> Result<(), SyncError> {
        self.state.clear()?;
        Ok(())
    }

    /// Alle aktuell lokal bekannten Pfade (für Tests).
    pub fn known_paths(&self) -> Result<BTreeSet<String>, SyncError> {
        Ok(self.state.all()?.into_iter().map(|s| s.path).collect())
    }
}

fn local_unchanged(s: &Synced, m: &fs::Metadata) -> bool {
    s.size == m.len() as i64 && s.local_mtime == mtime_ms(m)
}

pub fn hash_file(p: &Path) -> std::io::Result<String> {
    let mut f = fs::File::open(p)?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
    }
    Ok(hex::encode(h.finalize()))
}

fn mtime_ms(m: &fs::Metadata) -> i64 {
    m.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

/// Lokaler relativer Pfad -> Server-Pfad mit "/". None bei Nicht-UTF-8.
fn to_remote(rel: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for c in rel.components() {
        parts.push(c.as_os_str().to_str()?);
    }
    Some(parts.join("/"))
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

pub fn human_size(b: i64) -> String {
    let b = b as f64;
    if b < 1024.0 {
        format!("{b} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1024.0 / 1024.0).replace('.', ",")
    } else {
        format!("{:.1} GB", b / 1024.0 / 1024.0 / 1024.0).replace('.', ",")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignore_rules() {
        assert!(is_ignored("~$Bericht.docx"));
        assert!(is_ignored("desktop.ini"));
        assert!(is_ignored(".dropcraft-123.tmp"));
        assert!(!is_ignored("Bericht.docx"));
    }

    #[test]
    fn parallel_runs_concurrently_and_stops_on_fatal() {
        use std::sync::atomic::AtomicUsize;
        let (now, max) = (AtomicUsize::new(0), AtomicUsize::new(0));
        let mut seen = 0;
        parallel(
            (0..12).collect(),
            PARALLEL,
            |_| {
                let n = now.fetch_add(1, Ordering::SeqCst) + 1;
                max.fetch_max(n, Ordering::SeqCst);
                std::thread::sleep(std::time::Duration::from_millis(30));
                now.fetch_sub(1, Ordering::SeqCst);
            },
            |_, _| {
                seen += 1;
                false
            },
        );
        assert_eq!(seen, 12);
        assert_eq!(max.load(Ordering::SeqCst), PARALLEL);

        // Schwerer Fehler: keine neuen Aufträge, laufende werden noch gemeldet.
        let mut seen = 0;
        parallel((0..40).collect::<Vec<i32>>(), 2, |_| std::thread::sleep(std::time::Duration::from_millis(5)), |_, _| {
            seen += 1;
            true
        });
        assert!(seen < 40, "nach dem Fehler wurde weitergemacht: {seen}");
    }

    #[test]
    fn ext_split() {
        assert_eq!(split_ext("a.tar.gz"), ("a.tar", ".gz"));
        assert_eq!(split_ext(".bashrc"), (".bashrc", ""));
        assert_eq!(split_ext("README"), ("README", ""));
    }
}
