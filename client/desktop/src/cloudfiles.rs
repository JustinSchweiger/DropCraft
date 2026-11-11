//! Windows-Cloud-Files: Platzhalter („nur online“), Abruf beim Öffnen,
//! Status-Spalte im Explorer, „Immer auf diesem Gerät behalten“.
//!
//! Nur aktiv mit `cargo build --features cloud-files` unter Windows.

use std::{
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::mpsc::Sender,
};

use cloud_filter::{
    error::{CResult, CloudErrorKind},
    filter::{info, ticket, Request, SyncFilter},
    metadata::Metadata,
    placeholder::{ConvertOptions, Placeholder, UpdateOptions},
    placeholder_create_info::PlaceholderCreateInfo,
    root::{Connection, HydrationType, PopulationType, SecurityId, Session, SyncRootIdBuilder, SyncRootInfo},
    utility::WriteAt,
};
use dropcraft_core::{progress, Api, LocalHooks, ProgressRef, RemoteEntry};
use nt_time::FileTime;

use crate::{config, worker::Cmd};

const ATTR_REPARSE_POINT: u32 = 0x0000_0400;
const ATTR_PINNED: u32 = 0x0008_0000;
const ATTR_UNPINNED: u32 = 0x0010_0000;
const ATTR_RECALL_ON_DATA_ACCESS: u32 = 0x0040_0000;

fn attrs(p: &Path) -> u32 {
    std::fs::symlink_metadata(p).map(|m| m.file_attributes()).unwrap_or(0)
}

fn io_err(e: impl std::fmt::Display) -> std::io::Error {
    std::io::Error::other(e.to_string())
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
    let rel = local.strip_prefix(root).ok()?;
    let parts: Option<Vec<&str>> = rel.components().map(|c| c.as_os_str().to_str()).collect();
    Some(parts?.join("/"))
}

// ================================================================ Hooks für die Sync-Engine

pub struct CloudHooks {
    root: PathBuf,
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

    fn ensure_placeholder(&self, l: &Path, is_dir: bool) -> std::io::Result<Placeholder> {
        let mut ph = Placeholder::options().write_access().open(l).map_err(io_err)?;
        if !Self::is_placeholder(l) {
            let blob = remote_of(&self.root, l).unwrap_or_default().into_bytes();
            let mut opts = ConvertOptions::default().mark_in_sync().blob(blob);
            if is_dir {
                opts = opts.has_children();
            }
            ph.convert_to_placeholder(opts, None).map_err(io_err)?;
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
        let mut ph = self.ensure_placeholder(l, false)?;
        ph.update(UpdateOptions::default().metadata(metadata_for(e)).dehydrate().mark_in_sync().blob(&blob), None)
            .map_err(io_err)?;
        Ok(true)
    }

    fn free_space(&self, l: &Path) -> std::io::Result<()> {
        let mut ph = self.ensure_placeholder(l, false)?;
        ph.update(UpdateOptions::default().dehydrate().mark_in_sync(), None).map_err(io_err)?;
        Ok(())
    }

    fn mark_synced(&self, l: &Path, is_dir: bool) {
        let res = self.ensure_placeholder(l, is_dir).and_then(|mut ph| ph.mark_in_sync(true, None).map(|_| ()).map_err(io_err));
        if let Err(e) = res {
            log::warn!("In-Sync-Markierung für {} fehlgeschlagen: {e}", l.display());
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
    fn full_path(&self, req: &Request) -> PathBuf {
        let mut s = req.volume_letter();
        s.push(req.path().as_os_str());
        PathBuf::from(s)
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
        let range = info.required_file_range();
        // Windows verlangt 4-KiB-ausgerichtete Stücke (außer am Dateiende).
        let start = range.start & !4095;
        let end = ((range.end + 4095) & !4095).min(size);
        if end <= start {
            return Ok(());
        }

        progress::start(&self.progress, &remote, false, end - start);
        let mut buf: Vec<u8> = Vec::with_capacity(1 << 20);
        let mut written = start;
        let res = self.api.download_range(&remote, start, end - start, &mut |_, chunk| {
            buf.extend_from_slice(chunk);
            progress::advance(&self.progress, chunk.len() as u64);
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
        progress::finish(&self.progress, res.is_ok());
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
        return Err("Windows unterstützt hier keine Cloud-Dateien".into());
    }
    let id = root_id(account_id)?;
    if !id.is_registered().map_err(|e| e.to_string())? {
        let icon = config::app_dir().join("folder.ico");
        let info = SyncRootInfo::default()
            .with_display_name(format!("DropCraft – {label}"))
            .with_hydration_type(HydrationType::Full)
            .with_population_type(PopulationType::AlwaysFull)
            .with_allow_pinning(true)
            .with_version(env!("CARGO_PKG_VERSION"))
            .with_icon(format!("{},0", icon.display()))
            .with_path(root)
            .map_err(|e| e.to_string())?;
        id.register(info).map_err(|e| format!("Registrierung fehlgeschlagen: {e}"))?;
    }
    let provider = Provider { root: root.to_path_buf(), api, progress, wake };
    let conn = Session::new().connect(root, provider).map_err(|e| format!("Verbindung fehlgeschlagen: {e}"))?;
    Ok((CloudHooks { root: root.to_path_buf() }, CloudRoot { _conn: conn }))
}

/// Meldet den Sync-Ordner ab (beim Entfernen eines Kontos). Vorher werden
/// alle Dateien heruntergeladen, damit nichts verschwindet.
pub fn unregister(account_id: &str, root: &Path) {
    for entry in walkdir_files(root) {
        if attrs(&entry) & ATTR_RECALL_ON_DATA_ACCESS != 0 {
            if let Ok(mut ph) = Placeholder::open(&entry) {
                let _ = ph.hydrate(..);
            }
        }
    }
    if let Ok(id) = root_id(account_id) {
        let _ = id.unregister();
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
