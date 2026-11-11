use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// Ein Konto: eigener Server, eigene Anmeldung, eigener Sync-Ordner.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub server: String,
    pub username: String,
    pub device_name: String,
    pub sync_dir: PathBuf,
    pub token: Option<String>,
    #[serde(default)]
    pub paused: bool,
}

impl Account {
    /// Kurzer Name für Reiter und Menüs, z. B. „ocv · cloud2.example.de“.
    pub fn label(&self) -> String {
        format!("{} · {}", self.username, self.host())
    }

    pub fn host(&self) -> &str {
        self.server.trim_start_matches("https://").trim_start_matches("http://")
    }

    pub fn state_dir(&self) -> PathBuf {
        app_dir().join("accounts").join(&self.id)
    }

    pub fn state_db(&self) -> PathBuf {
        self.state_dir().join("state.db")
    }
}

/// Einstellungen in %APPDATA%\DropCraft\config.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub accounts: Vec<Account>,
    #[serde(default = "yes")]
    pub autostart: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self { accounts: Vec::new(), autostart: true }
    }
}

fn yes() -> bool {
    true
}

/// Format der ersten Version (ein einziges Konto).
#[derive(Deserialize)]
struct LegacyConfig {
    server: String,
    username: String,
    device_name: String,
    sync_dir: PathBuf,
    token: Option<String>,
    #[serde(default)]
    paused: bool,
    #[serde(default = "yes")]
    autostart: bool,
}

pub fn app_dir() -> PathBuf {
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("DropCraft")
}

pub fn new_account_id() -> String {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{:012x}", nanos & 0xffff_ffff_ffff)
}

impl Config {
    pub fn load() -> Config {
        let path = app_dir().join("config.json");
        let Ok(s) = std::fs::read_to_string(&path) else { return Config::default() };
        if let Ok(c) = serde_json::from_str::<Config>(&s) {
            return c;
        }
        // Alte Einzelkonto-Datei übernehmen, Sync-Stand mitnehmen.
        if let Ok(old) = serde_json::from_str::<LegacyConfig>(&s) {
            let acc = Account {
                id: "konto1".into(),
                server: old.server,
                username: old.username,
                device_name: old.device_name,
                sync_dir: old.sync_dir,
                token: old.token,
                paused: old.paused,
            };
            let _ = std::fs::create_dir_all(acc.state_dir());
            for ext in ["", "-wal", "-shm"] {
                let from = app_dir().join(format!("state.db{ext}"));
                if from.exists() {
                    let _ = std::fs::rename(&from, acc.state_dir().join(format!("state.db{ext}")));
                }
            }
            let cfg = Config { accounts: vec![acc], autostart: old.autostart };
            let _ = cfg.save();
            return cfg;
        }
        Config::default()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(app_dir())?;
        let tmp = app_dir().join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, app_dir().join("config.json"))
    }

    pub fn default_sync_dir(username: &str, first: bool) -> PathBuf {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
        if first || username.trim().is_empty() {
            home.join("DropCraft")
        } else {
            home.join(format!("DropCraft {}", username.trim()))
        }
    }

    pub fn default_device_name() -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }

    /// Prüft, ob ein Ordner sich mit dem eines anderen Kontos überschneidet.
    pub fn overlapping_account(&self, dir: &Path, except_id: Option<&str>) -> Option<&Account> {
        let d = norm(dir);
        self.accounts
            .iter()
            .filter(|a| Some(a.id.as_str()) != except_id)
            .find(|a| {
                let o = norm(&a.sync_dir);
                d == o || d.starts_with(&format!("{o}/")) || o.starts_with(&format!("{d}/"))
            })
    }
}

/// Pfad zum Vergleichen: Trenner vereinheitlicht, unter Windows ohne Groß/klein.
fn norm(p: &Path) -> String {
    let s = p.to_string_lossy().replace('\\', "/");
    let s = s.trim_end_matches('/').to_string();
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// „Mit Windows starten“ über den Run-Schlüssel des Benutzers.
#[cfg(windows)]
pub fn set_autostart(enabled: bool) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER).create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run") else {
        return;
    };
    if enabled {
        if let Ok(exe) = std::env::current_exe() {
            let _ = key.set_value("DropCraft", &format!("\"{}\" --autostart", exe.display()));
        }
    } else {
        let _ = key.delete_value("DropCraft");
    }
}

#[cfg(not(windows))]
pub fn set_autostart(_enabled: bool) {}

/// Explorer-Rechtsklick „DropCraft-Link kopieren“ für Dateien und Ordner in
/// den Sync-Ordnern. Unter Windows 11 im Menü „Weitere Optionen anzeigen“.
#[cfg(windows)]
pub fn register_context_menu(accounts: &[Account]) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(exe) = std::env::current_exe() else { return };
    for base in ["Software\\Classes\\Directory\\shell\\DropCraftShare", "Software\\Classes\\*\\shell\\DropCraftShare"] {
        if accounts.is_empty() {
            let _ = hkcu.delete_subkey_all(base);
            continue;
        }
        let Ok((key, _)) = hkcu.create_subkey(base) else { continue };
        let _ = key.set_value("MUIVerb", &"DropCraft-Link kopieren");
        let _ = key.set_value("Icon", &format!("\"{}\",0", exe.display()));
        // Kein „AppliesTo“-Filter: der war unzuverlässig. Liegt die Datei
        // außerhalb der Sync-Ordner, meldet die App das freundlich.
        let _ = key.delete_value("AppliesTo");
        if let Ok((cmd, _)) = key.create_subkey("command") {
            let _ = cmd.set_value("", &format!("\"{}\" --share \"%1\"", exe.display()));
        }
    }
}

#[cfg(not(windows))]
pub fn register_context_menu(_accounts: &[Account]) {}

#[cfg(test)]
mod tests {
    use super::*;

    fn acc(dir: &str) -> Account {
        Account {
            id: dir.into(),
            server: "s".into(),
            username: "u".into(),
            device_name: "d".into(),
            sync_dir: dir.into(),
            token: None,
            paused: false,
        }
    }

    #[test]
    fn overlap() {
        let c = Config { accounts: vec![acc("/home/j/DropCraft")], autostart: true };
        assert!(c.overlapping_account(Path::new("/home/j/DropCraft"), None).is_some());
        assert!(c.overlapping_account(Path::new("/home/j/DropCraft/Sub"), None).is_some());
        assert!(c.overlapping_account(Path::new("/home/j"), None).is_some());
        assert!(c.overlapping_account(Path::new("/home/j/DropCraft OCV"), None).is_none());
    }
}
