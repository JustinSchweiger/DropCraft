//! Englisch und Deutsch. Englisch ist Standard; die Sprache steht in der
//! Konfiguration (`language`: "en" oder "de") und gilt auch für den Kern.

pub use dropcraft_core::lang::{german, t};

/// Sprache aus der Konfiguration übernehmen.
pub fn apply(code: &str) {
    dropcraft_core::lang::set_german(code == "de");
}

/// `tf!("English {}", "Deutsch {}", wert)`: format! in der gewählten Sprache.
macro_rules! tf {
    ($en:literal, $de:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::german() {
            format!($de $(, $arg)*)
        } else {
            format!($en $(, $arg)*)
        }
    };
}
