//! HTTP-Client für die DropCraft-Server-API.

use std::{fs::File, io::Write, path::Path, time::Duration};

use reqwest::{blocking::Client, StatusCode};
use serde::{Deserialize, Serialize};

#[derive(Debug, thiserror::Error)]
pub enum ApiError {
    #[error("Nicht angemeldet oder Sitzung abgelaufen")]
    Unauthorized,
    #[error("Konflikt: {0}")]
    Conflict(String),
    #[error("Nicht gefunden")]
    NotFound,
    #[error("Speicherplatz voll")]
    QuotaExceeded,
    #[error("Server meldet Fehler {status}: {message}")]
    Server { status: u16, message: String },
    #[error("Server nicht erreichbar: {0}")]
    Network(String),
    #[error("Dateifehler: {0}")]
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
        let file = File::open(local)?;
        let len = file.metadata()?.len();
        let body = reqwest::blocking::Body::sized(file, len);
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
        let mut resp = Self::check(
            self.auth(self.http.get(self.url("/files/download")))
                .query(&[("path", remote_path)])
                .send()?,
        )?;
        let mut out = File::create(dest)?;
        resp.copy_to(&mut out)?;
        out.flush()?;
        out.sync_all()?;
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

    pub fn delete(&self, remote_path: &str) -> ApiResult<()> {
        Self::check(
            self.auth(self.http.post(self.url("/files/delete")))
                .json(&serde_json::json!({ "path": remote_path }))
                .send()?,
        )?;
        Ok(())
    }
}
