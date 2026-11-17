//! HTTP-Client für die DropCraft-Server-API.

use std::{fs::File, io::Write, path::Path, time::Duration};

use reqwest::{blocking::Client, StatusCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("{}", crate::lang::t("Not signed in or session expired", "Nicht angemeldet oder Sitzung abgelaufen"))]
    Unauthorized,
    #[error("{}: {0}", crate::lang::t("Conflict", "Konflikt"))]
    Conflict(String),
    #[error("{}", crate::lang::t("Not found", "Nicht gefunden"))]
    NotFound,
    #[error("{}", crate::lang::t("Storage full", "Speicherplatz voll"))]
    QuotaExceeded,
    #[error("{} {status}: {message}", crate::lang::t("Server error", "Server meldet Fehler"))]
    Server { status: u16, message: String },
    #[error("{}: {0}", crate::lang::t("Server not reachable", "Server nicht erreichbar"))]
    Network(String),
    #[error("{}: {0}", crate::lang::t("File error", "Dateifehler"))]
    Io(#[from] std::io::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl From<reqwest::Error> for ApiError {
    fn from(e: reqwest::Error) -> Self {
        ApiError::Network(e.to_string())
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct RemoteEntry {
    pub path: String,
    pub is_dir: bool,
    pub size: i64,
    pub sha256: Option<String>,
    pub modified_at: i64,
    pub rev: i64,
    pub deleted: bool,
    /// Gerät oder Gast, das die letzte Änderung gemacht hat.
    #[serde(default)]
    pub modified_by: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct Changes {
    pub entries: Vec<RemoteEntry>,
    pub cursor: i64,
    pub has_more: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Me {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub quota_bytes: i64,
    pub used_bytes: i64,
    pub cursor: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UserInfo {
    pub id: i64,
    pub username: String,
    pub is_admin: bool,
    pub quota_bytes: i64,
    pub used_bytes: i64,
    pub created_at: i64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Share {
    pub token: String,
    pub path: String,
    pub created_at: i64,
    #[serde(default)]
    pub allow_upload: bool,
    #[serde(default)]
    pub has_password: bool,
    #[serde(default)]
    pub expires_at: Option<i64>,
    #[serde(default = "yes")]
    pub notify: bool,
}

fn yes() -> bool {
    true
}

/// Änderungen an einer Freigabe; `None` = unverändert.
#[derive(Debug, Clone, Default, Serialize)]
pub struct ShareSettings {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub allow_upload: Option<bool>,
    /// Neues Passwort, "" entfernt es.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub password: Option<String>,
    /// Ablauf in Unix-ms, 0 entfernt ihn.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub notify: Option<bool>,
}

#[derive(Clone)]
pub struct Api {
    base: String,
    token: Option<String>,
    http: Client,
}

impl Api {
    pub fn new(server: &str, token: Option<String>) -> ApiResult<Self> {
        let base = format!("{}/api", server.trim_end_matches('/'));
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(15))
            // Kein Gesamt-Timeout: große Uploads/Downloads dürfen dauern.
            .timeout(None)
            .user_agent(concat!("DropCraft/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self { base, token, http })
    }

    /// Server-Adresse ohne /api, z. B. für Freigabe-Links.
    pub fn server(&self) -> &str {
        self.base.trim_end_matches("/api")
    }

    pub fn share_url(&self, token: &str) -> String {
        format!("{}/s/{token}", self.server())
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    fn url(&self, p: &str) -> String {
        format!("{}{}", self.base, p)
    }

    fn auth(&self, rb: reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder {
        match &self.token {
            Some(t) => rb.bearer_auth(t),
            None => rb,
        }
    }

    fn check(resp: reqwest::blocking::Response) -> ApiResult<reqwest::blocking::Response> {
        let status = resp.status();
        if status.is_success() {
            return Ok(resp);
        }
        let message = resp
            .json::<serde_json::Value>()
            .ok()
            .and_then(|v| v.get("message").and_then(|m| m.as_str()).map(String::from))
            .unwrap_or_default();
        Err(match status {
            StatusCode::UNAUTHORIZED => ApiError::Unauthorized,
            StatusCode::CONFLICT => ApiError::Conflict(message),
            StatusCode::NOT_FOUND => ApiError::NotFound,
            StatusCode::INSUFFICIENT_STORAGE => ApiError::QuotaExceeded,
            s => ApiError::Server { status: s.as_u16(), message },
        })
    }

    pub fn health(&self) -> ApiResult<()> {
        Self::check(self.http.get(self.url("/health")).send()?)?;
        Ok(())
    }

    /// Meldet sich an und gibt einen Client mit Token zurück.
    pub fn login(server: &str, username: &str, password: &str, device_name: &str) -> ApiResult<Api> {
        #[derive(Deserialize)]
        struct Resp {
            token: String,
        }
        let api = Api::new(server, None)?;
        let resp = Self::check(
            api.http
                .post(api.url("/auth/login"))
                .json(&serde_json::json!({ "username": username, "password": password, "device_name": device_name }))
                .send()?,
        )?;
        let r: Resp = resp.json()?;
        Api::new(server, Some(r.token))
    }

    pub fn logout(&self) -> ApiResult<()> {
        Self::check(self.auth(self.http.post(self.url("/auth/logout"))).send()?)?;
        Ok(())
    }

    pub fn me(&self) -> ApiResult<Me> {
        Ok(Self::check(self.auth(self.http.get(self.url("/me"))).send()?)?.json()?)
    }

    pub fn changes(&self, cursor: i64) -> ApiResult<Changes> {
        Ok(Self::check(
            self.auth(self.http.get(self.url("/sync/changes")))
                .query(&[("cursor", cursor)])
                .send()?,
        )?
        .json()?)
    }

    /// Lädt eine Datei hoch. `base_rev`: 0 = darf noch nicht existieren,
    /// sonst die Revision, auf der die lokale Änderung basiert.
    pub fn upload(&self, remote_path: &str, local: &Path, mtime_ms: i64, base_rev: i64) -> ApiResult<RemoteEntry> {
        self.upload_with_progress(remote_path, local, mtime_ms, base_rev, |_| Ok(()))
    }

    /// Wie `upload`, ruft `on_bytes` für jedes gelesene Stück auf.
    pub fn upload_with_progress(
        &self,
        remote_path: &str,
        local: &Path,
        mtime_ms: i64,
        base_rev: i64,
        on_bytes: impl Fn(u64) -> std::io::Result<()> + Send + 'static,
    ) -> ApiResult<RemoteEntry> {
        let file = File::open(local)?;
        let len = file.metadata()?.len();
        let reader = CountingReader { inner: file, on_bytes };
        let body = reqwest::blocking::Body::sized(reader, len);
        let resp = self
            .auth(self.http.put(self.url("/files/upload")))
            .query(&[("path", remote_path)])
            .query(&[("mtime", mtime_ms), ("base_rev", base_rev)])
            .body(body)
            .send()?;
        Ok(Self::check(resp)?.json()?)
    }

    /// Lädt eine Datei nach `dest` herunter (überschreibt).
    pub fn download(&self, remote_path: &str, dest: &Path) -> ApiResult<()> {
        self.download_with_progress(remote_path, dest, &mut |_| Ok(()))
    }

    pub fn download_with_progress(&self, remote_path: &str, dest: &Path, on_bytes: &mut dyn FnMut(u64) -> std::io::Result<()>) -> ApiResult<()> {
        let mut resp = Self::check(
            self.auth(self.http.get(self.url("/files/download")))
                .query(&[("path", remote_path)])
                .send()?,
        )?;
        let mut out = std::io::BufWriter::with_capacity(1 << 20, File::create(dest)?);
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = std::io::Read::read(&mut resp, &mut buf).map_err(|e| ApiError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n])?;
            on_bytes(n as u64)?;
        }
        let f = out.into_inner().map_err(|e| ApiError::Io(e.into_error()))?;
        f.sync_all()?;
        Ok(())
    }

    pub fn mkdir(&self, remote_path: &str) -> ApiResult<RemoteEntry> {
        Ok(Self::check(
            self.auth(self.http.post(self.url("/files/mkdir")))
                .json(&serde_json::json!({ "path": remote_path }))
                .send()?,
        )?
        .json()?)
    }

    // ------------------------------------------------ Freigaben

    pub fn create_share(&self, remote_path: &str) -> ApiResult<Share> {
        Ok(Self::check(
            self.auth(self.http.post(self.url("/shares")))
                .json(&serde_json::json!({ "path": remote_path }))
                .send()?,
        )?
        .json()?)
    }

    pub fn list_shares(&self) -> ApiResult<Vec<Share>> {
        Ok(Self::check(self.auth(self.http.get(self.url("/shares"))).send()?)?.json()?)
    }

    pub fn update_share(&self, token: &str, settings: &ShareSettings) -> ApiResult<Share> {
        Ok(Self::check(
            self.auth(self.http.patch(self.url(&format!("/shares/{token}"))))
                .json(settings)
                .send()?,
        )?
        .json()?)
    }

    pub fn delete_share(&self, token: &str) -> ApiResult<()> {
        Self::check(self.auth(self.http.delete(self.url(&format!("/shares/{token}")))).send()?)?;
        Ok(())
    }

    /// Teilbereich einer Datei laden (für den Abruf von Platzhaltern).
    pub fn download_range(&self, remote_path: &str, offset: u64, len: u64, on_chunk: &mut dyn FnMut(u64, &[u8]) -> std::io::Result<()>) -> ApiResult<()> {
        let end = offset + len.saturating_sub(1);
        let mut resp = Self::check(
            self.auth(self.http.get(self.url("/files/download")))
                .query(&[("path", remote_path)])
                .header(reqwest::header::RANGE, format!("bytes={offset}-{end}"))
                .send()?,
        )?;
        let partial = resp.status() == StatusCode::PARTIAL_CONTENT;
        let mut pos: u64 = 0; // Position im Antwortkörper
        let mut buf = vec![0u8; 256 * 1024];
        loop {
            let n = std::io::Read::read(&mut resp, &mut buf).map_err(|e| ApiError::Network(e.to_string()))?;
            if n == 0 {
                break;
            }
            // Ohne Range-Unterstützung kommt die ganze Datei: zuschneiden.
            let (abs_start, chunk) = if partial {
                (offset + pos, &buf[..n])
            } else {
                let s = pos;
                let e = pos + n as u64;
                if e <= offset || s > end {
                    pos += n as u64;
                    continue;
                }
                let from = offset.saturating_sub(s) as usize;
                let to = ((end + 1).min(e) - s) as usize;
                (s + from as u64, &buf[from..to])
            };
            pos += n as u64;
            on_chunk(abs_start, chunk)?;
        }
        Ok(())
    }

    // ------------------------------------------------ Benutzerverwaltung (Admin)

    /// Einmal-Link zur Konto-Website, gleich angemeldet (`page`: "admin" oder leer).
    pub fn web_login_url(&self, page: &str) -> ApiResult<String> {
        let v: serde_json::Value = Self::check(
            self.auth(self.http.post(self.url("/account/web-login")))
                .json(&serde_json::json!({ "page": page }))
                .send()?,
        )?
        .json()?;
        let path = v.get("url").and_then(|u| u.as_str()).unwrap_or("/account");
        Ok(format!("{}{path}", self.server()))
    }

    pub fn list_users(&self) -> ApiResult<Vec<UserInfo>> {
        Ok(Self::check(self.auth(self.http.get(self.url("/admin/users"))).send()?)?.json()?)
    }

    pub fn create_user(&self, username: &str, password: &str, quota_gb: f64, is_admin: bool) -> ApiResult<()> {
        Self::check(
            self.auth(self.http.post(self.url("/admin/users")))
                .json(&serde_json::json!({ "username": username, "password": password, "quota_gb": quota_gb, "is_admin": is_admin }))
                .send()?,
        )?;
        Ok(())
    }

    pub fn update_user(&self, id: i64, quota_gb: Option<f64>, password: Option<&str>) -> ApiResult<()> {
        let mut body = serde_json::Map::new();
        if let Some(q) = quota_gb {
            body.insert("quota_gb".into(), q.into());
        }
        if let Some(p) = password {
            body.insert("password".into(), p.into());
        }
        Self::check(
            self.auth(self.http.patch(self.url(&format!("/admin/users/{id}"))))
                .json(&body)
                .send()?,
        )?;
        Ok(())
    }

    pub fn delete_user(&self, id: i64) -> ApiResult<()> {
        Self::check(self.auth(self.http.delete(self.url(&format!("/admin/users/{id}")))).send()?)?;
        Ok(())
    }

    pub fn delete(&self, remote_path: &str) -> ApiResult<()> {
        Self::check(
            self.auth(self.http.post(self.url("/files/delete")))
                .json(&serde_json::json!({ "path": remote_path }))
                .send()?,
        )?;
        Ok(())
    }
}

struct CountingReader<R, F> {
    inner: R,
    on_bytes: F,
}

impl<R: std::io::Read, F: Fn(u64) -> std::io::Result<()>> std::io::Read for CountingReader<R, F> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let n = self.inner.read(buf)?;
        (self.on_bytes)(n as u64)?;
        Ok(n)
    }
}
