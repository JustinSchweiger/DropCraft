//! Live-Fortschritt der Übertragungen für die Oberfläche.

use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Transfer {
    pub path: String,
    pub upload: bool,
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone)]
pub struct QueueItem {
    pub path: String,
    pub upload: bool,
    pub size: u64,
}

#[derive(Debug, Clone, Default)]
pub struct Progress {
    /// Datei, die gerade übertragen wird.
    pub current: Option<Transfer>,
    /// Dateien, die in diesem Durchgang noch kommen (ohne `current`).
    pub queue: Vec<QueueItem>,
    /// In diesem Durchgang fertig übertragene Dateien.
    pub files_done: usize,
    /// Zähler seit Programmstart (steigt nur), für die Geschwindigkeit.
    pub bytes_up: u64,
    pub bytes_down: u64,
    /// Ereignisse des laufenden Durchgangs (sofort sichtbar, nicht erst am Ende).
    pub events: Vec<crate::sync::Event>,
}

impl Progress {
    pub fn bytes_total(&self) -> u64 {
        self.bytes_up + self.bytes_down
    }

    /// Noch zu übertragende Bytes inklusive Rest der aktuellen Datei.
    pub fn remaining_bytes(&self) -> u64 {
        let q: u64 = self.queue.iter().map(|q| q.size).sum();
        q + self.current.as_ref().map(|c| c.total.saturating_sub(c.done)).unwrap_or(0)
    }

    pub fn remaining_files(&self) -> usize {
        self.queue.len() + usize::from(self.current.is_some())
    }

    pub fn is_active(&self) -> bool {
        self.current.is_some() || !self.queue.is_empty()
    }
}

pub type ProgressRef = Arc<Mutex<Progress>>;

pub fn enqueue(p: &ProgressRef, path: &str, upload: bool, size: u64) {
    let mut g = p.lock().unwrap();
    if !g.queue.iter().any(|q| q.path == path && q.upload == upload) {
        g.queue.push(QueueItem { path: path.to_string(), upload, size });
    }
}

pub fn start(p: &ProgressRef, path: &str, upload: bool, total: u64) {
    let mut g = p.lock().unwrap();
    g.queue.retain(|q| !(q.path == path && q.upload == upload));
    g.current = Some(Transfer { path: path.to_string(), upload, done: 0, total });
}

pub fn advance(p: &ProgressRef, n: u64) {
    let mut g = p.lock().unwrap();
    let upload = match g.current.as_mut() {
        Some(c) => {
            c.done += n;
            c.upload
        }
        None => return,
    };
    if upload {
        g.bytes_up += n;
    } else {
        g.bytes_down += n;
    }
}

pub fn finish(p: &ProgressRef, ok: bool) {
    let mut g = p.lock().unwrap();
    g.current = None;
    if ok {
        g.files_done += 1;
    }
}

pub fn drop_queued(p: &ProgressRef, path: &str, upload: bool) {
    p.lock().unwrap().queue.retain(|q| !(q.path == path && q.upload == upload));
}
