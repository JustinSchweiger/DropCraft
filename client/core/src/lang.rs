//! Sprache für Texte aus dem Kern (Fehler, Ereignisse, Konflikt-Kopien).
//! Englisch ist Standard; die App schaltet beim Start auf ihre Einstellung um.

use std::sync::atomic::{AtomicBool, Ordering};

static GERMAN: AtomicBool = AtomicBool::new(false);

pub fn set_german(on: bool) {
    GERMAN.store(on, Ordering::Relaxed);
}

pub fn german() -> bool {
    GERMAN.load(Ordering::Relaxed)
}

/// Englischer oder deutscher Text, je nach Einstellung.
pub fn t<'a>(en: &'a str, de: &'a str) -> &'a str {
    if german() {
        de
    } else {
        en
    }
}
