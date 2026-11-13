//! Web-App unter `/app` (für iPhone & Co. „Zum Home-Bildschirm“) und
//! zeitlich begrenzte Download-Links, damit Browser Videos direkt abspielen
//! können (ohne Authorization-Header).

use axum::{
    extract::{Path, Query, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use serde_json::json;
use sha2::{Digest, Sha256};

use crate::{
    auth::AuthUser,
    db::now_ms,
    error::{ApiError, ApiResult},
    files::{get_live_pool, serve_blob},
    paths, AppState,
};

/// Gültigkeit eines Download-Links.
const LINK_TTL_MS: i64 = 6 * 3600 * 1000;

fn sign(secret: &str, user: i64, exp: i64, path: &str) -> String {
    let mut h = Sha256::new();
    h.update(secret.as_bytes());
    h.update(format!(":dl:{user}:{exp}:{path}:").as_bytes());
    h.update(secret.as_bytes());
    hex::encode(&h.finalize()[..16])
}

#[derive(Deserialize)]
pub struct LinkQuery {
    path: String,
}

/// `GET /api/files/link?path=…` → `{ "url": "/dl?…" }`
pub async fn link(State(state): State<AppState>, user: AuthUser, Query(q): Query<LinkQuery>) -> ApiResult<Json<serde_json::Value>> {
    let path = paths::normalize_non_root(&q.path)?;
    let entry = get_live_pool(&state, user.id, &path).await?.ok_or(ApiError::NotFound)?;
    if entry.is_dir {
        return Err(ApiError::BadRequest("Ordner können nicht heruntergeladen werden".into()));
    }
    let exp = now_ms() + LINK_TTL_MS;
    let sig = sign(&state.secret, user.id, exp, &path);
    let url = format!("/dl?u={}&e={exp}&s={sig}&p={}", user.id, crate::files::pct_encode(&path));
    Ok(Json(json!({ "url": url, "expires_at": exp })))
}

#[derive(Deserialize)]
pub struct DlQuery {
    u: i64,
    e: i64,
    s: String,
    p: String,
    #[serde(default)]
    download: Option<String>,
}

/// `GET /dl?u&e&s&p` – liefert die Datei (mit Range) aus.
pub async fn dl(State(state): State<AppState>, Query(q): Query<DlQuery>, headers: HeaderMap) -> Response {
    let Ok(path) = paths::normalize_non_root(&q.p) else { return StatusCode::BAD_REQUEST.into_response() };
    if q.e < now_ms() || sign(&state.secret, q.u, q.e, &path) != q.s {
        return (StatusCode::FORBIDDEN, "Link abgelaufen").into_response();
    }
    match get_live_pool(&state, q.u, &path).await {
        Ok(Some(entry)) if !entry.is_dir => match serve_blob(&state, q.u, &entry, &headers, q.download.is_some()).await {
            Ok(r) => r,
            Err(e) => e.into_response(),
        },
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

// ---------------------------------------------------------------- Updates

/// Dateien, die die CI nach jedem Build in `data/public` legt.
const UPDATE_FILES: &[(&str, &str)] = &[
    ("latest.json", "application/json"),
    ("DropCraft-Windows.exe", "application/vnd.microsoft.portable-executable"),
    ("DropCraft-Windows-ohne-Cloud-Dateien.exe", "application/vnd.microsoft.portable-executable"),
    ("DropCraft-macOS.zip", "application/zip"),
    ("DropCraft-iOS.ipa", "application/octet-stream"),
];

/// `GET /updates/{datei}` – Update-Infos und App-Downloads (öffentlich).
pub async fn update_file(State(state): State<AppState>, Path(name): Path<String>) -> Response {
    let Some((name, mime)) = UPDATE_FILES.iter().find(|(n, _)| *n == name) else { return StatusCode::NOT_FOUND.into_response() };
    let path = state.cfg.data_dir.join("public").join(name);
    let (Ok(meta), Ok(file)) = (tokio::fs::metadata(&path).await, tokio::fs::File::open(&path).await) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let disposition = if name.ends_with(".json") { "inline".to_string() } else { format!("attachment; filename=\"{name}\"") };
    (
        [
            (header::CONTENT_TYPE, mime.to_string()),
            (header::CONTENT_LENGTH, meta.len().to_string()),
            (header::CONTENT_DISPOSITION, disposition),
            (header::CACHE_CONTROL, "no-cache".to_string()),
        ],
        axum::body::Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response()
}

// ---------------------------------------------------------------- statische Dateien

const CSP: &str = "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' blob: data:; \
media-src 'self' blob:; connect-src 'self'; manifest-src 'self'; worker-src 'self'; frame-ancestors 'none'; base-uri 'none'";

fn asset(name: &str) -> Option<(&'static str, &'static [u8])> {
    Some(match name {
        "" | "index.html" => ("text/html; charset=utf-8", include_bytes!("../assets/app/index.html")),
        "app.js" => ("text/javascript; charset=utf-8", include_bytes!("../assets/app/app.js")),
        "app.css" => ("text/css; charset=utf-8", include_bytes!("../assets/app/app.css")),
        "sw.js" => ("text/javascript; charset=utf-8", include_bytes!("../assets/app/sw.js")),
        "manifest.webmanifest" => ("application/manifest+json", include_bytes!("../assets/app/manifest.webmanifest")),
        "icon-180.png" => ("image/png", include_bytes!("../assets/app/icon-180.png")),
        "icon-192.png" => ("image/png", include_bytes!("../assets/app/icon-192.png")),
        "icon-512.png" => ("image/png", include_bytes!("../assets/app/icon-512.png")),
        _ => return None,
    })
}

fn serve(name: &str) -> Response {
    match asset(name) {
        Some((mime, bytes)) => (
            [
                (header::CONTENT_TYPE, mime),
                // Kurzer Cache; der Service Worker hält die App offline bereit.
                (header::CACHE_CONTROL, "no-cache"),
                (header::CONTENT_SECURITY_POLICY, CSP),
                (header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
            ],
            bytes,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

pub async fn index() -> Response {
    serve("index.html")
}

pub async fn file(Path(name): Path<String>) -> Response {
    serve(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn signature_depends_on_everything() {
        let a = sign("geheim", 1, 100, "a.txt");
        assert_eq!(a, sign("geheim", 1, 100, "a.txt"));
        assert_ne!(a, sign("geheim", 2, 100, "a.txt"));
        assert_ne!(a, sign("geheim", 1, 101, "a.txt"));
        assert_ne!(a, sign("geheim", 1, 100, "b.txt"));
        assert_ne!(a, sign("anders", 1, 100, "a.txt"));
        assert!(asset("app.js").is_some() && asset("../Cargo.toml").is_none());
        assert!(UPDATE_FILES.iter().all(|(n, _)| !n.contains('/') && !n.contains("..")));
    }
}
