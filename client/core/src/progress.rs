//! Live-Fortschritt der Übertragungen für die Oberfläche.

use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct Transfer {
    /// Kennung, um parallele Übertragungen auseinanderzuhalten.
    pub id: u64,
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
    /// Dateien, die gerade übertragen werden (mehrere gleichzeitig).
    pub active: Vec<Transfer>,
    /// Dateien, die in diesem Durchgang noch kommen (ohne `active`).
    pub queue: Vec<QueueItem>,
    /// In diesem Durchgang fertig übertragene Dateien.
    pub files_done: usize,
    /// Zähler seit Programmstart (steigt nur), für die Geschwindigkeit.
    pub bytes_up: u64,
    pub bytes_down: u64,
    /// Ereignisse des laufenden Durchgangs (sofort sichtbar, nicht erst am Ende).
    pub events: Vec<crate::sync::Event>,
    next_id: u64,
}

impl Progress {
    pub fn bytes_total(&self) -> u64 {
        self.bytes_up + self.bytes_down
    }

    /// Noch zu übertragende Bytes inklusive Rest der laufenden Dateien.
    pub fn remaining_bytes(&self) -> u64 {
        let q: u64 = self.queue.iter().map(|q| q.size).sum();
        q + self.active.iter().map(|c| c.total.saturating_sub(c.done)).sum::<u64>()
    }

    pub fn remaining_files(&self) -> usize {
        self.queue.len() + self.active.len()
    }

    pub fn is_active(&self) -> bool {
        !self.active.is_empty() || !self.queue.is_empty()
    }

    /// Die zuerst gestartete laufende Übertragung.
    pub fn current(&self) -> Option<&Transfer> {
        self.active.first()
    }

    /// Wird gerade hoch- bzw. heruntergeladen?
    pub fn uploading(&self) -> bool {
        self.active.iter().any(|t| t.upload)
    }
    pub fn downloading(&self) -> bool {
        self.active.iter().any(|t| !t.upload)
    }

    /// Laufende Übertragungen und Warteschlange leeren (neuer Durchgang).
    pub fn reset_round(&mut self) {
        self.queue.clear();
        self.active.clear();
        self.files_done = 0;
        self.events.clear();
    }
}

pub type ProgressRef = Arc<Mutex<Progress>>;

pub fn enqueue(p: &ProgressRef, path: &str, upload: bool, size: u64) {
    let mut g = p.lock().unwrap();
    if !g.queue.iter().any(|q| q.path == path && q.upload == upload) {
        g.queue.push(QueueItem { path: path.to_string(), upload, size });
    }
}

/// Startet eine Übertragung und gibt ihre Kennung zurück.
pub fn start(p: &ProgressRef, path: &str, upload: bool, total: u64) -> u64 {
    let mut g = p.lock().unwrap();
    g.queue.retain(|q| !(q.path == path && q.upload == upload));
    g.next_id += 1;
    let id = g.next_id;
    g.active.push(Transfer { id, path: path.to_string(), upload, done: 0, total });
    id
}

pub fn advance(p: &ProgressRef, id: u64, n: u64) {
    let mut g = p.lock().unwrap();
    let upload = match g.active.iter_mut().find(|t| t.id == id) {
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

pub fn finish(p: &ProgressRef, id: u64, ok: bool) {
    let mut g = p.lock().unwrap();
    g.active.retain(|t| t.id != id);
    if ok {
        g.files_done += 1;
    }
}

pub fn drop_queued(p: &ProgressRef, path: &str, upload: bool) {
    p.lock().unwrap().queue.retain(|q| !(q.path == path && q.upload == upload));
}
