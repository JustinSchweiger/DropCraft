//! Windows-Cloud-Files: Platzhalter („nur online“), Abruf beim Öffnen,
//! Status-Spalte im Explorer, „Immer auf diesem Gerät behalten“.
//!
//! Nur aktiv mit `cargo build --features cloud-files` unter Windows.

use std::{
    collections::HashSet,
    os::windows::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::{mpsc::Sender, Mutex},
};

use cloud_filter::{
    error::{CResult, CloudErrorKind},
    filter::{info, ticket, Request, SyncFilter},
    metadata::Metadata,
    placeholder::{ConvertOptions, PinOptions, PinState, Placeholder, UpdateOptions},
    placeholder_create_info::PlaceholderCreateInfo,
    root::{Connection, HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder, SyncRootInfo},
    utility::WriteAt,
};
use dropcraft_core::{progress, Api, LocalHooks, ProgressRef, RemoteEntry};
use nt_time::FileTime;

use crate::worker::Cmd;

const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
const ATTR_PINNED: u32 = 0x0008_0000;
const ATTR_UNPINNED: u32 = 0x0010_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;
/// Nötig, um auch Ordner öffnen zu können.
const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;

/// Normales Win32-Handle ohne Datenzugriff (löst keinen Download aus).
/// `exclusive`: für das Freigeben des Inhalts (Dehydrieren) nötig.
fn open_handle(p: &Path, exclusive: bool) -> std::io::Result<Placeholder> {
    let mut o = std::fs::OpenOptions::new();
    o.access_mode(0).custom_flags(FILE_FLAG_BACKUP_SEMANTICS);
    if exclusive {
        o.share_mode(0);
    }
    Ok(Placeholder::from(o.open(p)?))
}

fn attrs(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).map(|m| m.file_attributes()).unwrap_or(0)
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    let text = e.to_string();
    // Win32-Fehlercode aus "… (0x8007XXXX)" übernehmen, damit z. B.
    // "Datei wird verwendet" (32) erkannt werden kann.
    if let Some(i) = text.find("0x8007") {
        if let Some(code) = text.get(i + 6..i + 10).and_then(|h| i32::from_str_radix(h, 16).ok()) {
            return std::io::Error::from_raw_os_error(code);
        }
    }
    std::io::Error::other(text)
}

fn file_time(ms: i64) -> FileTime {
    FileTime::from_unix_time_millis(ms).unwrap_or(FileTime::UNIX_EPOCH)
}

fn metadata_for(e: &RemoteEntry) -> Metadata {
    let t = file_time(e.modified_at);
    Metadata::file().size(e.size.max(0) as u64).created(t).written(t).changed(t).accessed(t)
}

/// Pfad im Sync-Ordner -> Server-Pfad ("a/b.txt").
fn remote_of(root: &Path, local: &Path) -> Option<String> {
    // Windows liefert Pfade teils in anderer Groß-/Kleinschreibung.
    let rel = match local.strip_prefix(root) {
        Ok(r) => r.to_path_buf(),
        Err(_) => {
            let (r, l) = (root.to_string_lossy().to_lowercase(), local.to_string_lossy());
            let r = r.trim_end_matches('\\');
            if !l.to_lowercase().starts_with(r) {
                return None;
            }
            let rest = l.get(r.len()..)?;
            if !(rest.is_empty() || rest.starts_with('\\')) {
                return None;
            }
            PathBuf::from(rest.trim_start_matches('\\'))
        }
    };
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

// ================================================================ Hooks für die Sync-Engine

pub struct CloudHooks {
    root: PathBuf,
    /// In diesem Programmlauf schon als synchron markiert.
    marked: Mutex<HashSet<PathBuf>>,
    /// Gesetzt, wenn Windows den Ordner nicht (mehr) als Sync-Stamm kennt;
    /// der Worker meldet ihn dann neu an.
    broken: std::sync::Arc<std::sync::atomic::AtomicBool>,
    account_id: String,
}

/// Windows-Fehler „nur unter einem Cloudsynchronisierungsstamm“ (0x80070186).
const ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT: i32 = 390;

impl CloudHooks {
    pub fn broken_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.broken.clone()
    }

    /// Macht aus einem Fehler eine verständliche Meldung und merkt sich,
    /// dass die Anmeldung bei Windows fehlt.
    fn check(&self, e: std::io::Error) -> std::io::Error {
        if e.raw_os_error() != Some(ERROR_CLOUD_FILE_NOT_UNDER_SYNC_ROOT) {
            return e;
        }
        self.broken.store(true, std::sync::atomic::Ordering::SeqCst);
        let ours = root_id(&self.account_id).map(|i| i.to_os_string().to_string_lossy().into_owned()).unwrap_or_default();
        let state = match cloud_filter::root::SyncRootId::from_path(&self.root) {
            Ok(id) if id.to_os_string().to_string_lossy() == ours => crate::i18n::t("is registered, but Windows still rejects the file", "ist angemeldet, Windows lehnt die Datei trotzdem ab").to_string(),
            Ok(id) => tf!("belongs to “{}”", "gehört zu „{}“", id.to_os_string().to_string_lossy()),
            Err(err) => tf!("is not registered with Windows as a cloud folder ({err})", "ist bei Windows nicht als Cloud-Ordner angemeldet ({err})"),
        };
        std::io::Error::new(
            e.kind(),
            tf!("{} – “{}” {state}. DropCraft registers it again.", "{} – „{}“ {state}. DropCraft meldet ihn neu an.", e, self.root.display()),
        )
    }
}

impl CloudHooks {
    /// Nächstgelegene ausdrückliche Einstellung (angepinnt / freigegeben)
    /// der Datei oder eines übergeordneten Ordners im Sync-Ordner.
    fn pin(&self, l: &Path) -> Option<bool> {
        for a in l.ancestors() {
            let at = attrs(a);
            if at & ATTR_PINNED != 0 {
                return Some(true);
            }
            if at & ATTR_UNPINNED != 0 {
                return Some(false);
            }
            if a == self.root {
                break;
            }
        }
        None
    }

    fn is_placeholder(l: &Path) -> bool {
        attrs(l) & ATTR_REPARSE_POINT != 0
    }

    /// Öffnet die Datei/den Ordner und macht sie bei Bedarf zum Platzhalter.
    fn ensure_placeholder(&self, l: &Path, exclusive: bool) -> std::io::Result<Placeholder> {
        let mut ph = open_handle(l, exclusive)?;
        if !Self::is_placeholder(l) {
            let blob = remote_of(&self.root, l).unwrap_or_default().into_bytes();
            ph.convert_to_placeholder(ConvertOptions::default().mark_in_sync().blob(blob), None).map_err(io_err)?;
        }
        Ok(ph)
    }
}

impl LocalHooks for CloudHooks {
    fn is_online_only(&self, l: &Path) -> bool {
        attrs(l) & ATTR_RECALL_ON_DATA_ACCESS != 0
    }

    fn wants_local(&self, l: &Path) -> bool {
        self.pin(l) == Some(true)
    }

    fn wants_free(&self, l: &Path) -> bool {
        self.pin(l) == Some(false)
    }

    fn create_placeholder(&self, l: &Path, e: &RemoteEntry) -> std::io::Result<bool> {
        let (Some(parent), Some(name)) = (l.parent(), l.file_name()) else { return Ok(false) };
        PlaceholderCreateInfo::new(name)
            .metadata(metadata_for(e))
            .mark_in_sync()
            .blob(e.path.clone().into_bytes())
            .create(parent)
            .map_err(io_err)?;
        Ok(true)
    }

    fn update_placeholder(&self, l: &Path, e: &RemoteEntry) -> std::io::Result<bool> {
        let blob = e.path.clone().into_bytes();
        let mut ph = self.ensure_placeholder(l, true)?;
        ph.update(UpdateOptions::default().metadata(metadata_for(e)).dehydrate().mark_in_sync().blob(&blob), None)
            .map_err(io_err)?;
        Ok(true)
    }

    fn hydrate(&self, l: &Path) -> std::io::Result<bool> {
        // Blockiert, bis fetch_data (Provider) den ganzen Inhalt geliefert hat.
        let mut ph = open_handle(l, false).map_err(|e| self.check(e))?;
        ph.hydrate(..).map_err(io_err).map_err(|e| self.check(e))?;
        Ok(true)
    }

    fn free_space(&self, l: &Path) -> std::io::Result<()> {
        let mut ph = self.ensure_placeholder(l, true)?;
        ph.update(UpdateOptions::default().dehydrate().mark_in_sync(), None).map_err(io_err)?;
        // Wie OneDrive: Nach dem Freigeben gilt die Datei wieder als „normal“.
        // Sonst würde sie nach jedem Öffnen sofort wieder freigegeben.
        if attrs(l) & ATTR_UNPINNED != 0 {
            let _ = ph.mark_pin(PinState::Unspecified, PinOptions::default());
        }
        Ok(())
    }

    fn mark_synced(&self, l: &Path, _is_dir: bool) {
        let res = self.ensure_placeholder(l, false).and_then(|mut ph| ph.mark_in_sync(true, None).map(|_| ()).map_err(io_err));
        match res {
            Ok(()) => {
                self.marked.lock().unwrap().insert(l.to_path_buf());
            }
            Err(e) => log::warn!("In-Sync-Markierung für {} fehlgeschlagen: {e}", l.display()),
        }
    }

    fn check_synced(&self, l: &Path, is_dir: bool) {
        if !self.marked.lock().unwrap().contains(l) {
            self.mark_synced(l, is_dir);
        }
    }
}

// ================================================================ Rückrufe von Windows

struct Provider {
    root: PathBuf,
    api: Api,
    progress: ProgressRef,
    wake: Sender<Cmd>,
}

impl Provider {
    /// Vollständiger Pfad der angefragten Datei. `Request::path()` enthält
    /// den Laufwerksbuchstaben bereits („C:\DropCraft\…“); früher wurde er
    /// doppelt vorangestellt („C:C:\…“) – dann passte nichts zum Sync-Ordner
    /// und jeder Abruf scheiterte mit Fehler 390.
    fn full_path(&self, req: &Request) -> PathBuf {
        let p = req.path();
        if p.has_root() && p.components().next().map(|c| matches!(c, std::path::Component::Prefix(_))).unwrap_or(false) {
            p
        } else {
            let mut s = req.volume_letter();
            s.push(p.as_os_str());
            PathBuf::from(s)
        }
    }
}

impl SyncFilter for Provider {
    /// Datei wird geöffnet, ihr Inhalt fehlt noch: vom Server holen.
    fn fetch_data(&self, request: Request, ticket: ticket::FetchData, info: info::FetchData) -> CResult<()> {
        let local = self.full_path(&request);
        let Some(remote) = remote_of(&self.root, &local) else {
            let _ = ticket.fail(CloudErrorKind::NotUnderSyncRoot);
            return Err(CloudErrorKind::NotUnderSyncRoot);
        };
        let size = request.file_size();
        let req = info.required_file_range();
        let opt = info.optional_file_range();
        // Volle Hydrierung: immer bis zum Dateiende liefern. Nur den
        // geforderten Ausschnitt zu liefern führt bei Videos (Player,
        // Vorschaubilder) zu ständig neuen Abrufen derselben Datei.
        // Windows verlangt 4-KiB-ausgerichtete Stücke (außer am Dateiende).
        let from = if opt.end > opt.start { req.start.min(opt.start) } else { req.start };
        let start = from & !4095;
        let end = size;
        if end <= start {
            return Ok(());
        }

        let pid = progress::start(&self.progress, &remote, false, end - start);
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
        let mut written = start;
        let res = self.api.download_range(&remote, start, end - start, &mut |_, chunk| {
            buf.extend_from_slice(chunk);
            progress::advance(&self.progress, pid, chunk.len() as u64);
            if buf.len() >= 1 << 20 {
                let n = buf.len() & !4095;
                ticket.write_at(&buf[..n], written).map_err(io_err)?;
                written += n as u64;
                buf.drain(..n);
                let _ = ticket.report_progress(end - start, written - start);
            }
            Ok(())
        });
        let res = res.map_err(|e| e.to_string()).and_then(|_| {
            if !buf.is_empty() {
                ticket.write_at(&buf, written).map_err(|e| e.to_string())?;
            }
            Ok(())
        });
        progress::finish(&self.progress, pid, res.is_ok());
        match res {
            Ok(()) => Ok(()),
            Err(e) => {
                log::warn!("Abruf von {remote} fehlgeschlagen: {e}");
                let _ = ticket.fail(CloudErrorKind::NetworkUnavailable);
                Err(CloudErrorKind::NetworkUnavailable)
            }
        }
    }

    /// Löschen erlauben; die Sync-Engine überträgt es danach zum Server.
    fn delete(&self, _request: Request, ticket: ticket::Delete, _info: info::Delete) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::InvalidRequest)?;
        let _ = self.wake.send(Cmd::FsChanged);
        Ok(())
    }

    /// Umbenennen/Verschieben erlauben.
    fn rename(&self, _request: Request, ticket: ticket::Rename, _info: info::Rename) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::InvalidRequest)?;
        let _ = self.wake.send(Cmd::FsChanged);
        Ok(())
    }

    /// „Speicherplatz freigeben“ erlauben.
    fn dehydrate(&self, _request: Request, ticket: ticket::Dehydrate, _info: info::Dehydrate) -> CResult<()> {
        ticket.pass().map_err(|_| CloudErrorKind::InvalidRequest)?;
        Ok(())
    }

    /// Lokale Änderungen und Pin-Wechsel: Sync anstoßen.
    fn state_changed(&self, _event: cloud_filter::filter::StateWatchEvent) {
        let _ = self.wake.send(Cmd::FsChanged);
    }
}

// ================================================================ Anmelden / Verbinden

pub struct CloudRoot {
    _conn: Connection<Provider>,
}

fn root_id(account_id: &str) -> Result<cloud_filter::root::SyncRootId, String> {
    let sid = SecurityId::current_user().map_err(|e| e.to_string())?;
    Ok(SyncRootIdBuilder::new("DropCraft").user_security_id(sid).account_name(account_id).build())
}

/// Registriert den Sync-Ordner bei Windows (einmalig) und verbindet sich.
/// Gibt die Hooks für die Sync-Engine und die Verbindung zurück.
pub fn connect(
    account_id: &str,
    label: &str,
    root: &Path,
    api: Api,
    progress: ProgressRef,
    wake: Sender<Cmd>,
) -> Result<(CloudHooks, CloudRoot), String> {
    if !cloud_filter::root::is_supported().unwrap_or(false) {
        return Err(crate::i18n::t("Windows does not support cloud files here", "Windows unterstützt hier keine Cloud-Dateien").into());
    }
    let id = root_id(account_id)?;
    // Alte DropCraft-Registrierungen für denselben Ordner entfernen (z. B.
    // nach erneutem Hinzufügen des Kontos). Zwei Registrierungen für einen
    // Ordner lassen die neue scheitern; Windows meldet dann bei jeder Datei
    // „nur unter einem Cloudsynchronisierungsstamm unterstützt“ (0x80070186).
    if let Ok(roots) = cloud_filter::root::active_roots() {
        let ours = id.to_os_string();
        let norm = |p: &Path| p.to_string_lossy().trim_end_matches('\\').to_lowercase();
        for r in roots {
            let rid = r.id();
            let theirs = rid.to_os_string();
            if theirs != ours && theirs.to_string_lossy().starts_with("DropCraft!") && norm(&r.path()) == norm(root) {
                let _ = rid.unregister();
            }
        }
    }
    // Immer neu registrieren: aktualisiert Name und Symbol einer bestehenden
    // Registrierung, Platzhalter bleiben erhalten.
    {
        let icon = crate::folder_icon::ico_path();
        let info = SyncRootInfo::default()
            .with_display_name(format!("DropCraft – {label}"))
            .with_hydration_type(HydrationType::Full)
            .with_population_type(PopulationType::AlwaysFull)
            .with_allow_pinning(true)
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_icon(format!("{},0", icon.display()))
            .with_path(root)
            .map_err(|e| e.to_string())?;
        id.register(info).map_err(|e| {
            tf!("Registration failed: {e}. Is the folder inside OneDrive or another cloud folder? Then choose another location.", "Registrierung fehlgeschlagen: {e}. Liegt der Ordner in OneDrive oder einem anderen Cloud-Ordner? Dann einen anderen Speicherort wählen.")
        })?;
    }
    let provider = Provider { root: root.to_path_buf(), api, progress, wake };
    let conn = Session::new().connect(root, provider).map_err(|e| tf!("Connection failed: {e}", "Verbindung fehlgeschlagen: {e}"))?;
    let hooks = CloudHooks {
        root: root.to_path_buf(),
        marked: Mutex::new(HashSet::new()),
        broken: Default::default(),
        account_id: account_id.to_string(),
    };
    // Prüfen, ob Windows den Ordner jetzt wirklich als unseren Sync-Stamm kennt.
    match cloud_filter::root::SyncRootId::from_path(root) {
        Ok(found) if found.to_os_string() == id.to_os_string() => {}
        Ok(found) => {
            return Err(tf!(
                "Windows has the folder registered for “{}”, not for DropCraft. Please choose another folder.",
                "Der Ordner ist bei Windows für „{}“ angemeldet, nicht für DropCraft. Bitte einen anderen Ordner wählen.",
                found.to_os_string().to_string_lossy()
            ))
        }
        Err(e) => return Err(tf!("Windows did not accept the folder as a cloud folder: {e}", "Windows hat den Ordner nicht als Cloud-Ordner übernommen: {e}")),
    }
    // Der Sync-Ordner selbst bekommt ebenfalls den grünen Haken.
    if let Ok(mut ph) = open_handle(root, false) {
        let _ = ph.mark_in_sync(true, None);
    }
    Ok((hooks, CloudRoot { _conn: conn }))
}

/// Meldet den Sync-Ordner ab (beim Entfernen eines Kontos). Vorher werden
/// alle Dateien heruntergeladen, damit nichts verschwindet.
pub fn unregister(account_id: &str, root: &Path) {
    for entry in walkdir_files(root) {
        if attrs(&entry) & ATTR_RECALL_ON_DATA_ACCESS != 0 {
            if let Ok(mut ph) = open_handle(&entry, false) {
                let _ = ph.hydrate(..);
            }
        }
    }
    if let Ok(id) = root_id(account_id) {
        let _ = id.unregister();
    }
}

/// Meldet alle DropCraft-Sync-Ordner dieses Benutzers ab (Deinstallation).
/// Ohne laufende App können Platzhalter nicht mehr geladen werden.
pub fn unregister_all() {
    if let Ok(roots) = cloud_filter::root::active_roots() {
        for r in roots {
            let id = r.id();
            if id.to_os_string().to_string_lossy().starts_with("DropCraft!") {
                let _ = id.unregister();
            }
        }
    }
}

fn walkdir_files(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else { continue };
        for e in rd.flatten() {
            let p = e.path();
            match e.file_type() {
                Ok(t) if t.is_dir() => stack.push(p),
                Ok(t) if t.is_file() => out.push(p),
                _ => {}
            }
        }
    }
    out
}
