//! Schnittstelle für „Dateien bei Bedarf“: Die Sync-Engine kann Dateien als
//! Platzhalter ohne Inhalt anlegen, statt sie herunterzuladen. Unter Windows
//! steckt dahinter die Cloud-Files-API (Status-Spalte im Explorer,
//! „Immer auf diesem Gerät behalten“); ohne Hooks wird alles geladen.

use std::path::Path;

use crate::api::RemoteEntry;

pub trait LocalHooks: Send {
    /// Ist die Datei nur als Platzhalter ohne Inhalt vorhanden?
    fn is_online_only(&self, _local: &Path) -> bool {
        false
    }

    /// Soll die Datei vollständig auf diesem Gerät liegen (z. B. angepinnt)?
    fn wants_local(&self, _local: &Path) -> bool {
        true
    }

    /// Soll der Inhalt wieder freigegeben werden („Speicherplatz freigeben“)?
    fn wants_free(&self, _local: &Path) -> bool {
        false
    }

    /// Legt einen Platzhalter für eine Server-Datei an. `Ok(false)`: nicht
    /// unterstützt, die Engine lädt die Datei dann ganz herunter.
    fn create_placeholder(&self, _local: &Path, _e: &RemoteEntry) -> std::io::Result<bool> {
        Ok(false)
    }

    /// Setzt eine (unveränderte) Datei auf die neue Server-Version und gibt
    /// ihren Inhalt frei. `Ok(false)`: nicht unterstützt.
    fn update_placeholder(&self, _local: &Path, _e: &RemoteEntry) -> std::io::Result<bool> {
        Ok(false)
    }

    /// Gibt den Inhalt einer synchronen Datei frei (wird zum Platzhalter).
    fn free_space(&self, _local: &Path) -> std::io::Result<()> {
        Ok(())
    }

    /// Datei/Ordner ist jetzt mit dem Server abgeglichen (grüner Haken).
    fn mark_synced(&self, _local: &Path, _is_dir: bool) {}

    /// Wird regelmäßig für alle abgeglichenen Einträge aufgerufen, damit auch
    /// ältere Dateien/Ordner markiert werden. Sollte sich Erledigtes merken.
    fn check_synced(&self, _local: &Path, _is_dir: bool) {}
}

/// Ohne Platzhalter: alles wird heruntergeladen.
pub struct FullDownload;

impl LocalHooks for FullDownload {}
