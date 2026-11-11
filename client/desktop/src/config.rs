use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// Einstellungen in %APPDATA%\DropCraft\config.json
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Config {
    pub server: String,
    pub username: String,
    pub device_name: String,
    pub sync_dir: PathBuf,
    pub token: Option<String>,
    #[serde(default)]
    pub paused: bool,
    #[serde(default = "yes")]
    pub autostart: bool,
}

fn yes() -> bool {
    true
}

pub fn app_dir() -> PathBuf {
    let base = dirs::config_dir().unwrap_or_else(|| PathBuf::from("."));
    base.join("DropCraft")
}

pub fn state_db_path() -> PathBuf {
    app_dir().join("state.db")
}

impl Config {
    pub fn load() -> Option<Config> {
        let s = std::fs::read_to_string(app_dir().join("config.json")).ok()?;
        serde_json::from_str(&s).ok()
    }

    pub fn save(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(app_dir())?;
        let tmp = app_dir().join("config.json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(self).unwrap_or_default())?;
        std::fs::rename(tmp, app_dir().join("config.json"))
    }

    pub fn is_logged_in(&self) -> bool {
        self.token.is_some()
    }

    pub fn default_sync_dir() -> PathBuf {
        dirs::home_dir().unwrap_or_else(|| PathBuf::from(".")).join("DropCraft")
    }

    pub fn default_device_name() -> String {
        gethostname::gethostname().to_string_lossy().into_owned()
    }
}

/// „Mit Windows starten“ über den Run-Schlüssel des Benutzers.
#[cfg(windows)]
pub fn set_autostart(enabled: bool) {
    use winreg::{enums::HKEY_CURRENT_USER, RegKey};
    let Ok((key, _)) = RegKey::predef(HKEY_CURRENT_USER)
        .create_subkey("Software\\Microsoft\\Windows\\CurrentVersion\\Run")
    else {
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
