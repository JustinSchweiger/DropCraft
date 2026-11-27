//! Benachrichtigungen in der App (statt Windows-Meldungen): Liste unter der
//! Glocke im Popup, ungelesene als Punkt am Taskleisten-Symbol.
//!
//! Gespeichert in `%APPDATA%\DropCraft\notifications.json`. Kurzlebige
//! Prozesse (z. B. „DropCraft-Link kopieren“ aus dem Explorer) schicken ihre
//! Meldung an die laufende App; läuft keine, landet sie direkt in der Datei.

use std::{
    io::Write,
    net::TcpStream,
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex, OnceLock,
    },
    time::Duration,
};

use serde::{Deserialize, Serialize};

/// Höchstens so viele Meldungen aufheben (die ältesten fallen weg).
const MAX: usize = 100;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Note {
    pub id: u64,
    /// Unix-Millisekunden
    pub at: i64,
    pub title: String,
    pub body: String,
    #[serde(default)]
    pub read: bool,
}

static NOTES: Mutex<Vec<Note>> = Mutex::new(Vec::new());
/// Läuft in diesem Prozess die App (mit Popup)?
static IN_APP: AtomicBool = AtomicBool::new(false);
static CTX: OnceLock<egui::Context> = OnceLock::new();

fn path() -> std::path::PathBuf {
    crate::config::app_dir().join("notifications.json")
}

fn load_file() -> Vec<Note> {
    std::fs::read_to_string(path()).ok().and_then(|t| serde_json::from_str(&t).ok()).unwrap_or_default()
}

fn save_file(notes: &[Note]) {
    let _ = std::fs::create_dir_all(crate::config::app_dir());
    if let Ok(text) = serde_json::to_string_pretty(notes) {
        let tmp = path().with_extension("json.tmp");
        if std::fs::write(&tmp, text).is_ok() {
            let _ = std::fs::rename(&tmp, path());
        }
    }
}

fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

/// Beim Start der App: gespeicherte Meldungen laden.
pub fn init_app(ctx: &egui::Context) {
    *NOTES.lock().unwrap() = load_file();
    IN_APP.store(true, Ordering::SeqCst);
    let _ = CTX.set(ctx.clone());
}

fn changed(notes: &[Note]) {
    save_file(notes);
    if let Some(ctx) = CTX.get() {
        ctx.request_repaint();
    }
}

fn insert(notes: &mut Vec<Note>, title: &str, body: &str, at: i64) {
    let id = notes.iter().map(|n| n.id).max().unwrap_or(0) + 1;
    notes.insert(0, Note { id, at, title: title.to_string(), body: body.to_string(), read: false });
    notes.truncate(MAX);
}

/// Neue Meldung – aus der App selbst oder aus einem anderen Prozess.
pub fn push(title: &str, body: &str) {
    crate::applog::line(&format!("Meldung: {title} – {body}"));
    if IN_APP.load(Ordering::SeqCst) {
        let mut notes = NOTES.lock().unwrap();
        insert(&mut notes, title, body, now_ms());
        changed(&notes);
        return;
    }
    // Anderer Prozess: an die laufende App schicken …
    let wire = serde_json::json!({ "title": title, "body": body, "at": now_ms() }).to_string();
    let sent = TcpStream::connect_timeout(&([127, 0, 0, 1], crate::INSTANCE_PORT).into(), Duration::from_millis(500))
        .and_then(|mut s| {
            s.write_all(format!("NOTE {wire}\n").as_bytes())?;
            s.shutdown(std::net::Shutdown::Write)
        })
        .is_ok();
    // … oder, wenn keine läuft, selbst in die Datei schreiben.
    if !sent {
        let mut notes = load_file();
        insert(&mut notes, title, body, now_ms());
        save_file(&notes);
    }
}

/// Von einem anderen Prozess empfangen (`NOTE {json}`).
pub fn receive(json: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else { return };
    let title = v["title"].as_str().unwrap_or("DropCraft");
    let body = v["body"].as_str().unwrap_or("");
    let at = v["at"].as_i64().unwrap_or_else(now_ms);
    let mut notes = NOTES.lock().unwrap();
    insert(&mut notes, title, body, at);
    changed(&notes);
}

pub fn list() -> Vec<Note> {
    NOTES.lock().unwrap().clone()
}

pub fn unread() -> usize {
    NOTES.lock().unwrap().iter().filter(|n| !n.read).count()
}

pub fn mark_all_read() {
    let mut notes = NOTES.lock().unwrap();
    if notes.iter().any(|n| !n.read) {
        notes.iter_mut().for_each(|n| n.read = true);
        changed(&notes);
    }
}

pub fn remove(id: u64) {
    let mut notes = NOTES.lock().unwrap();
    notes.retain(|n| n.id != id);
    changed(&notes);
}

pub fn clear() {
    let mut notes = NOTES.lock().unwrap();
    notes.clear();
    changed(&notes);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newest_first_and_capped() {
        let mut v = Vec::new();
        for i in 0..(MAX + 5) {
            insert(&mut v, &format!("t{i}"), "", i as i64);
        }
        assert_eq!(v.len(), MAX);
        assert_eq!(v[0].title, format!("t{}", MAX + 4));
        let ids: std::collections::HashSet<u64> = v.iter().map(|n| n.id).collect();
        assert_eq!(ids.len(), MAX, "IDs eindeutig");
    }
}
