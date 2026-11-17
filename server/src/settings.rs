//! Server-Einstellungen, die Admins auf der Konto-Website ändern. Sie liegen
//! in der Tabelle `meta` (Schlüssel `setting.*`); die Umgebungsvariablen aus
//! `deploy/.env` sind nur noch die Startwerte.

use crate::AppState;

#[derive(Clone, Debug)]
pub struct Settings {
    /// Name auf der Startseite und der Konto-Website.
    pub site_name: String,
    /// Jeder darf sich ein Konto anlegen (über die Apps).
    pub open_registration: bool,
    /// Speicher für neue Konten in GB.
    pub default_quota_gb: f64,
    /// Öffentliche Links erlaubt (aus = bestehende Links sind gesperrt).
    pub links_enabled: bool,
    /// Gäste dürfen in freigegebene Ordner hochladen (wo der Link es erlaubt).
    pub guest_upload: bool,
    /// Neue Links laufen nach so vielen Tagen ab (0 = nie).
    pub link_expiry_days: u32,
    /// Mindestlänge für Kontopasswörter.
    pub min_password_len: u32,
    /// Abmeldung auf der Konto-Website nach so vielen Tagen ohne Besuch.
    pub web_session_days: u32,
}

impl Settings {
    fn defaults(state: &AppState) -> Self {
        Settings {
            site_name: "DropCraft".into(),
            open_registration: state.cfg.open_registration,
            default_quota_gb: state.cfg.default_quota_bytes as f64 / GB,
            links_enabled: true,
            guest_upload: true,
            link_expiry_days: 0,
            min_password_len: 10,
            web_session_days: 30,
        }
    }

    pub fn default_quota_bytes(&self) -> i64 {
        (self.default_quota_gb * GB) as i64
    }
}

const GB: f64 = 1024.0 * 1024.0 * 1024.0;

pub async fn load(state: &AppState) -> Settings {
    let mut s = Settings::defaults(state);
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT key, value FROM meta WHERE key LIKE 'setting.%'")
        .fetch_all(&state.db)
        .await
        .unwrap_or_default();
    for (k, v) in rows {
        let b = v == "1";
        match k.trim_start_matches("setting.") {
            "site_name" if !v.trim().is_empty() => s.site_name = v,
            "open_registration" => s.open_registration = b,
            "default_quota_gb" => s.default_quota_gb = v.parse().unwrap_or(s.default_quota_gb),
            "links_enabled" => s.links_enabled = b,
            "guest_upload" => s.guest_upload = b,
            "link_expiry_days" => s.link_expiry_days = v.parse().unwrap_or(0),
            "min_password_len" => s.min_password_len = v.parse().unwrap_or(10).clamp(6, 128),
            "web_session_days" => s.web_session_days = v.parse().unwrap_or(30).clamp(1, 365),
            _ => {}
        }
    }
    s
}

pub async fn save(state: &AppState, s: &Settings) -> anyhow::Result<()> {
    let b = |v: bool| if v { "1".to_string() } else { "0".to_string() };
    let pairs = [
        ("site_name", s.site_name.trim().to_string()),
        ("open_registration", b(s.open_registration)),
        ("default_quota_gb", s.default_quota_gb.to_string()),
        ("links_enabled", b(s.links_enabled)),
        ("guest_upload", b(s.guest_upload)),
        ("link_expiry_days", s.link_expiry_days.to_string()),
        ("min_password_len", s.min_password_len.to_string()),
        ("web_session_days", s.web_session_days.to_string()),
    ];
    let mut tx = state.db.begin().await?;
    for (k, v) in pairs {
        sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value")
            .bind(format!("setting.{k}"))
            .bind(v)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}
