//! Selbst-Update: fragt `latest.json` auf dropcraft.tech ab, lädt die passende
//! Datei, prüft die Prüfsumme, tauscht das Programm aus und startet neu.

use std::{
    collections::HashMap,
    path::Path,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::Deserialize;

/// Woher Updates kommen (von der CI nach jedem grünen Build aktualisiert).
pub const UPDATE_URL: &str = "https://dropcraft.tech/updates/latest.json";

/// Version dieses Programms („0.1.<Build-Nummer>“ aus der CI).
pub fn version() -> &'static str {
    option_env!("DROPCRAFT_VERSION").unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Unterstützt diese Plattform Updates aus der App heraus?
pub const SUPPORTED: bool = cfg!(any(windows, target_os = "macos"));

#[derive(Debug, Clone, Deserialize)]
pub struct FileInfo {
    pub name: String,
    pub sha256: String,
    pub size: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Latest {
    pub version: String,
    #[serde(default)]
    pub notes: String,
    pub files: HashMap<String, FileInfo>,
}

impl Latest {
    /// Datei für genau diese Variante (Windows mit/ohne Cloud-Dateien, Mac).
    pub fn file(&self) -> Option<&FileInfo> {
        let key = if cfg!(target_os = "macos") {
            "macos"
        } else if cfg!(feature = "cloud-files") {
            "windows"
        } else {
            "windows-standard"
        };
        self.files.get(key)
    }
}

#[derive(Debug, Clone, Default)]
pub enum Status {
    #[default]
    Idle,
    Checking,
    UpToDate,
    Available(Latest),
    /// Fortschritt 0…1
    Downloading(f32),
    /// Neustart steht unmittelbar bevor.
    Restarting,
    /// Installation fehlgeschlagen (wird in der Aktivität gezeigt).
    Failed(String),
    /// Suche fehlgeschlagen, z. B. offline (nur in den Einstellungen).
    CheckFailed(String),
}

pub type StatusRef = Arc<Mutex<Status>>;

/// „0.1.12“ > „0.1.9“
pub fn newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| -> Vec<u64> { v.trim_start_matches('v').split(['.', '-', '+']).map(|p| p.parse().unwrap_or(0)).collect() };
    let (a, b) = (parse(candidate), parse(current));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (a.get(i).copied().unwrap_or(0), b.get(i).copied().unwrap_or(0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .user_agent(format!("DropCraft/{}", version()))
        .connect_timeout(Duration::from_secs(10))
        .timeout(None)
        .build()
        .map_err(|e| e.to_string())
}

/// Neue Version vorhanden? `None` = aktuell.
pub fn check() -> Result<Option<Latest>, String> {
    let resp = client()?.get(UPDATE_URL).timeout(Duration::from_secs(20)).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Update-Server antwortet mit {}", resp.status()));
    }
    let latest: Latest = resp.json().map_err(|e| e.to_string())?;
    Ok((newer(&latest.version, version()) && latest.file().is_some()).then_some(latest))
}

/// Lädt das Update, tauscht das Programm aus und startet es neu.
/// Kehrt nur im Fehlerfall zurück.
pub fn install(latest: &Latest, on_progress: impl Fn(f32)) -> Result<(), String> {
    let file = latest.file().ok_or("Für diese Version gibt es keine Update-Datei")?;
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let dir = exe.parent().ok_or("Programmordner nicht gefunden")?.to_path_buf();
    let tmp = dir.join(format!(".dropcraft-update-{}", std::process::id()));
    download(&format!("https://dropcraft.tech/updates/{}", file.name), &tmp, file.size, &on_progress)?;
    let sha = dropcraft_core::sync::hash_file(&tmp).map_err(|e| e.to_string())?;
    if !sha.eq_ignore_ascii_case(&file.sha256) {
        let _ = std::fs::remove_file(&tmp);
        return Err("Prüfsumme des Updates stimmt nicht".into());
    }
    replace_and_restart(&exe, &tmp)
}

fn download(url: &str, dest: &Path, size: u64, on_progress: &impl Fn(f32)) -> Result<(), String> {
    use std::io::{Read, Write};
    let mut resp = client()?.get(url).send().map_err(|e| e.to_string())?;
    if !resp.status().is_success() {
        return Err(format!("Download fehlgeschlagen ({})", resp.status()));
    }
    let mut out = std::fs::File::create(dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    let mut buf = vec![0u8; 256 * 1024];
    let mut done = 0u64;
    loop {
        let n = resp.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        out.write_all(&buf[..n]).map_err(|e| e.to_string())?;
        done += n as u64;
        if size > 0 {
            on_progress((done as f32 / size as f32).min(1.0));
        }
    }
    out.flush().map_err(|e| e.to_string())
}

#[cfg(windows)]
fn replace_and_restart(exe: &Path, new: &Path) -> Result<(), String> {
    // Eine laufende .exe darf umbenannt, aber nicht überschrieben werden.
    let old = exe.with_extension("exe.old");
    let _ = std::fs::remove_file(&old);
    std::fs::rename(exe, &old).map_err(|e| format!("Programm umbenennen: {e}"))?;
    if let Err(e) = std::fs::rename(new, exe) {
        let _ = std::fs::rename(&old, exe);
        return Err(format!("Update einsetzen: {e}"));
    }
    std::process::Command::new(exe).arg("--after-update").spawn().map_err(|e| format!("Neustart: {e}"))?;
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn replace_and_restart(exe: &Path, zip: &Path) -> Result<(), String> {
    // exe = …/DropCraft.app/Contents/MacOS/DropCraft
    let bundle: std::path::PathBuf = exe.ancestors().nth(3).ok_or("App-Bundle nicht gefunden")?.to_path_buf();
    let parent = bundle.parent().ok_or("App-Ordner nicht gefunden")?;
    let unpack = parent.join(format!(".dropcraft-update-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&unpack);
    let ok = std::process::Command::new("ditto")
        .args(["-x", "-k"])
        .arg(zip)
        .arg(&unpack)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let _ = std::fs::remove_file(zip);
    let new_bundle = unpack.join("DropCraft.app");
    if !ok || !new_bundle.exists() {
        let _ = std::fs::remove_dir_all(&unpack);
        return Err("Update-Archiv konnte nicht entpackt werden".into());
    }
    let old = parent.join(".DropCraft.app.old");
    let _ = std::fs::remove_dir_all(&old);
    std::fs::rename(&bundle, &old).map_err(|e| format!("App umbenennen: {e}"))?;
    if let Err(e) = std::fs::rename(&new_bundle, &bundle) {
        let _ = std::fs::rename(&old, &bundle);
        return Err(format!("Update einsetzen: {e}"));
    }
    let _ = std::fs::remove_dir_all(&unpack);
    std::process::Command::new("open").arg("-n").arg(&bundle).args(["--args", "--after-update"]).spawn().map_err(|e| format!("Neustart: {e}"))?;
    std::process::exit(0);
}

#[cfg(not(any(windows, target_os = "macos")))]
fn replace_and_restart(_exe: &Path, new: &Path) -> Result<(), String> {
    let _ = std::fs::remove_file(new);
    Err("Updates aus der App gibt es nur unter Windows und macOS".into())
}

/// Reste eines früheren Updates entfernen.
pub fn cleanup() {
    let Ok(exe) = std::env::current_exe() else { return };
    #[cfg(windows)]
    let _ = std::fs::remove_file(exe.with_extension("exe.old"));
    #[cfg(target_os = "macos")]
    if let Some(parent) = exe.ancestors().nth(4) {
        let _ = std::fs::remove_dir_all(parent.join(".DropCraft.app.old"));
    }
    if let Some(dir) = exe.parent() {
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                if e.file_name().to_string_lossy().starts_with(".dropcraft-update-") {
                    let _ = std::fs::remove_file(e.path()).or_else(|_| std::fs::remove_dir_all(e.path()));
                }
            }
        }
    }
}

/// Prüft im Hintergrund beim Start und dann alle 6 Stunden.
pub fn spawn_checker(status: StatusRef, ctx: egui::Context, auto_install: impl Fn() -> bool + Send + 'static, idle: impl Fn() -> bool + Send + 'static) {
    if !SUPPORTED {
        return;
    }
    let _ = std::thread::Builder::new().name("update".into()).spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        let mut notified: Option<String> = None;
        loop {
            check_now(&status, &ctx);
            let latest = match &*status.lock().unwrap() {
                Status::Available(l) => Some(l.clone()),
                _ => None,
            };
            if let Some(l) = latest {
                if auto_install() {
                    // Nicht mitten in Übertragungen neu starten.
                    for _ in 0..120 {
                        if idle() {
                            break;
                        }
                        std::thread::sleep(Duration::from_secs(30));
                    }
                    run_install(&status, &ctx, &l);
                } else if notified.as_deref() != Some(l.version.as_str()) {
                    crate::notify::show("DropCraft-Update verfügbar", &format!("Version {} kann in den Einstellungen installiert werden.", l.version));
                    notified = Some(l.version.clone());
                }
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        }
    });
}

pub fn check_now(status: &StatusRef, ctx: &egui::Context) {
    *status.lock().unwrap() = Status::Checking;
    ctx.request_repaint();
    let s = match check() {
        Ok(Some(l)) => Status::Available(l),
        Ok(None) => Status::UpToDate,
        Err(e) => Status::CheckFailed(e),
    };
    *status.lock().unwrap() = s;
    ctx.request_repaint();
}

/// Installiert (blockiert; im Erfolgsfall endet das Programm).
pub fn run_install(status: &StatusRef, ctx: &egui::Context, latest: &Latest) {
    *status.lock().unwrap() = Status::Downloading(0.0);
    ctx.request_repaint();
    let st = status.clone();
    let c = ctx.clone();
    let res = install(latest, move |p| {
        *st.lock().unwrap() = Status::Downloading(p);
        c.request_repaint();
    });
    *status.lock().unwrap() = match res {
        Ok(()) => Status::Restarting,
        Err(e) => Status::Failed(e),
    };
    ctx.request_repaint();
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn versions() {
        assert!(newer("0.1.12", "0.1.9"));
        assert!(newer("v0.2.0", "0.1.99"));
        assert!(!newer("0.1.9", "0.1.9"));
        assert!(!newer("0.1.8", "0.1.9"));
        assert!(newer("0.1.1", "0.1"));
    }
}
