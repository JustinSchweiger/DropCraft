//! Freigabe-Links: Ein Ordner (oder eine Datei) wird über eine öffentliche
//! Webseite /s/<token> lesbar, ohne dass man DropCraft braucht.

use std::io::Write;

use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Response},
    Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};

use crate::{
    auth::AuthUser,
    db::now_ms,
    error::{ApiError, ApiResult},
    files::{self, Entry},
    paths, AppState,
};

pub const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS shares (
    token      TEXT PRIMARY KEY,
    user_id    INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    path       TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (user_id, path)
);
"#;

#[derive(Serialize, sqlx::FromRow)]
pub struct Share {
    pub token: String,
    pub path: String,
    pub created_at: i64,
}

#[derive(Deserialize)]
pub struct CreateReq {
    path: String,
}

/// Legt einen Link an (oder gibt den bestehenden für diesen Pfad zurück).
pub async fn create(State(state): State<AppState>, user: AuthUser, Json(req): Json<CreateReq>) -> ApiResult<Json<Share>> {
    let path = paths::normalize_non_root(&req.path)?;
    if files::get_live_pool(&state, user.id, &path).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    if let Some(s) = sqlx::query_as::<_, Share>("SELECT token, path, created_at FROM shares WHERE user_id = ? AND path = ?")
        .bind(user.id)
        .bind(&path)
        .fetch_optional(&state.db)
        .await?
    {
        return Ok(Json(s));
    }
    let mut raw = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let created_at = now_ms();
    sqlx::query("INSERT INTO shares (token, user_id, path, created_at) VALUES (?, ?, ?, ?)")
        .bind(&token)
        .bind(user.id)
        .bind(&path)
        .bind(created_at)
        .execute(&state.db)
        .await?;
    Ok(Json(Share { token, path, created_at }))
}

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<Share>>> {
    Ok(Json(
        sqlx::query_as("SELECT token, path, created_at FROM shares WHERE user_id = ? ORDER BY created_at DESC")
            .bind(user.id)
            .fetch_all(&state.db)
            .await?,
    ))
}

pub async fn delete(State(state): State<AppState>, user: AuthUser, Path(token): Path<String>) -> ApiResult<Json<serde_json::Value>> {
    let r = sqlx::query("DELETE FROM shares WHERE token = ? AND user_id = ?")
        .bind(&token)
        .bind(user.id)
        .execute(&state.db)
        .await?;
    if r.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

// ------------------------------------------------------------ öffentlich

#[derive(Deserialize)]
pub struct PubQuery {
    #[serde(default)]
    p: String,
    #[serde(default)]
    inline: Option<u8>,
}

struct Resolved {
    user_id: i64,
    /// Freigegebener Pfad (Wurzel der Freigabe)
    root: String,
    /// Pfad relativ zur Freigabe
    rel: String,
    entry: Entry,
}

async fn resolve(state: &AppState, token: &str, rel: &str) -> Result<Resolved, Response> {
    let row: Option<(i64, String)> = sqlx::query_as("SELECT user_id, path FROM shares WHERE token = ?")
        .bind(token)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::from(e).into_response())?;
    let Some((user_id, root)) = row else { return Err(not_found_page()) };
    let rel = paths::normalize(rel).map_err(|_| not_found_page())?;
    let full = if rel.is_empty() { root.clone() } else { format!("{root}/{rel}") };
    let entry = files::get_live_pool(state, user_id, &full)
        .await
        .map_err(|e| e.into_response())?
        .ok_or_else(not_found_page)?;
    Ok(Resolved { user_id, root, rel, entry })
}

pub async fn page(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>) -> Response {
    let r = match resolve(&state, &token, &q.p).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    let root_name = r.root.rsplit('/').next().unwrap_or(&r.root).to_string();
    let base = format!("/s/{token}");

    // Brotkrumen
    let mut crumbs = format!(r#"<a href="{base}">{}</a>"#, esc(&root_name));
    let mut acc = String::new();
    for part in r.rel.split('/').filter(|p| !p.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        crumbs.push_str(&format!(r#"<span class="sep">/</span><a href="{base}?p={}">{}</a>"#, url_q(&acc), esc(part)));
    }

    let title = if r.rel.is_empty() { root_name.clone() } else { r.rel.rsplit('/').next().unwrap_or("").to_string() };
    let mut body = String::new();
    if r.entry.is_dir {
        let full = r.entry.path.clone();
        let items = match files::children(&state, r.user_id, &full).await {
            Ok(i) => i,
            Err(e) => return e.into_response(),
        };
        let files_count = items.iter().filter(|e| !e.is_dir).count();
        let total: i64 = items.iter().map(|e| e.size).sum();
        body.push_str(&format!(
            r#"<div class="bar"><span class="meta">{} Ordner · {} Dateien · {}</span><a class="btn primary" href="{base}/zip?p={}">{}Alles als ZIP</a></div>"#,
            items.len() - files_count,
            files_count,
            human(total),
            url_q(&r.rel),
            ICON_DOWNLOAD
        ));
        if items.is_empty() {
            body.push_str(r#"<p class="empty">Dieser Ordner ist leer.</p>"#);
        }
        body.push_str(r#"<ul class="list">"#);
        for e in &items {
            let name = e.path.rsplit('/').next().unwrap_or("");
            let rel = if r.rel.is_empty() { name.to_string() } else { format!("{}/{}", r.rel, name) };
            if e.is_dir {
                body.push_str(&format!(
                    r#"<li><a class="row" href="{base}?p={}"><span class="ic dir">{ICON_FOLDER}</span><span class="name">{}</span><span class="meta">Ordner</span></a></li>"#,
                    url_q(&rel),
                    esc(name)
                ));
            } else {
                let view = if is_viewable(name) {
                    format!(r#"<a class="btn" href="{base}/file?p={}&amp;inline=1" target="_blank" rel="noopener">Ansehen</a>"#, url_q(&rel))
                } else {
                    String::new()
                };
                body.push_str(&format!(
                    r#"<li><div class="row"><span class="ic">{}</span><span class="name">{}</span><span class="meta">{} · {}</span>{view}<a class="btn" href="{base}/file?p={}" aria-label="{} herunterladen">{ICON_DOWNLOAD}</a></div></li>"#,
                    if is_image(name) { ICON_IMAGE } else { ICON_FILE },
                    esc(name),
                    human(e.size),
                    date(e.modified_at),
                    url_q(&rel),
                    esc(name)
                ));
            }
        }
        body.push_str("</ul>");
    } else {
        let name = r.entry.path.rsplit('/').next().unwrap_or("");
        body.push_str(&format!(
            r#"<div class="single"><span class="ic big">{}</span><div class="name">{}</div><div class="meta">{} · {}</div><div class="actions">{}<a class="btn primary" href="{base}/file?p={}">{ICON_DOWNLOAD}Herunterladen</a></div></div>"#,
            if is_image(name) { ICON_IMAGE } else { ICON_FILE },
            esc(name),
            human(r.entry.size),
            date(r.entry.modified_at),
            if is_viewable(name) {
                format!(r#"<a class="btn" href="{base}/file?p={}&amp;inline=1" target="_blank" rel="noopener">Ansehen</a>"#, url_q(&r.rel))
            } else {
                String::new()
            },
            url_q(&r.rel)
        ));
    }

    let html = format!(
        r#"<!doctype html><html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<meta name="robots" content="noindex"><title>{} · DropCraft</title><style>{CSS}</style></head>
<body><header><div class="brand">{ICON_CUBE}<span>DROPCRAFT</span></div><span class="tag">Geteilter Ordner</span></header>
<main><nav class="crumbs">{crumbs}</nav><h1>{}</h1>{body}</main>
<footer>Geteilt mit DropCraft</footer></body></html>"#,
        esc(&title),
        esc(&title)
    );
    let mut resp = Html(html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static("default-src 'none'; style-src 'unsafe-inline'; img-src 'self' data:; frame-ancestors 'none'"),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    resp
}

pub async fn file(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>, headers: HeaderMap) -> Response {
    let r = match resolve(&state, &token, &q.p).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if r.entry.is_dir {
        return not_found_page();
    }
    let name = r.entry.path.rsplit('/').next().unwrap_or("");
    // Nur ungefährliche Typen inline anzeigen (kein HTML/SVG -> kein Skript).
    let inline = q.inline == Some(1) && is_viewable(name);
    match files::serve_blob(&state, r.user_id, &r.entry, &headers, !inline).await {
        Ok(mut resp) => {
            resp.headers_mut().insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
            resp
        }
        Err(e) => e.into_response(),
    }
}

/// Ganzer Ordner als ZIP (ohne Kompression, gestreamt).
pub async fn zip(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>) -> Response {
    let r = match resolve(&state, &token, &q.p).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !r.entry.is_dir {
        return not_found_page();
    }
    let dir = r.entry.path.clone();
    let list = match files::files_below(&state, r.user_id, &dir).await {
        Ok(l) => l,
        Err(e) => return e.into_response(),
    };
    let blob_dir = state.user_blob_dir(r.user_id);
    let zip_name = format!("{}.zip", dir.rsplit('/').next().unwrap_or("DropCraft"));
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Vec<u8>, std::io::Error>>(8);

    tokio::task::spawn_blocking(move || {
        let writer = ChannelWriter { tx: tx.clone(), buf: Vec::with_capacity(1 << 20) };
        let mut zip = zip::ZipWriter::new_stream(writer);
        let opts = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let prefix_len = dir.len() + 1;
        for e in list {
            let Some(blob) = &e.blob_id else { continue };
            let name = &e.path[prefix_len.min(e.path.len())..];
            let Ok(mut f) = std::fs::File::open(blob_dir.join(blob)) else { continue };
            if zip.start_file(name, opts).is_err() {
                return;
            }
            if std::io::copy(&mut f, &mut zip).is_err() {
                return; // Client hat abgebrochen
            }
        }
        if let Ok(mut w) = zip.finish() {
            let _ = w.flush();
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static("application/zip"));
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{}", files::pct_encode(&zip_name))) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    (headers, Body::from_stream(stream)).into_response()
}

/// Schreibt in einen Kanal; das ZIP entsteht so ohne Zwischendatei.
struct ChannelWriter {
    tx: tokio::sync::mpsc::Sender<Result<Vec<u8>, std::io::Error>>,
    buf: Vec<u8>,
}

impl Write for ChannelWriter {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.buf.extend_from_slice(data);
        if self.buf.len() >= 1 << 20 {
            self.flush()?;
        }
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let chunk = std::mem::replace(&mut self.buf, Vec::with_capacity(1 << 20));
        self.tx
            .blocking_send(Ok(chunk))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "Download abgebrochen"))
    }
}

impl Drop for ChannelWriter {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

// ------------------------------------------------------------ Hilfen

fn not_found_page() -> Response {
    let html = format!(
        r#"<!doctype html><html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1"><title>Nicht gefunden · DropCraft</title><style>{CSS}</style></head>
<body><header><div class="brand">{ICON_CUBE}<span>DROPCRAFT</span></div></header><main><h1>Link nicht gefunden</h1><p class="empty">Diese Freigabe gibt es nicht mehr oder der Link ist falsch.</p></main></body></html>"#
    );
    (StatusCode::NOT_FOUND, Html(html)).into_response()
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;").replace('\'', "&#39;")
}

fn url_q(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' | b'/' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn human(b: i64) -> String {
    let b = b as f64;
    if b < 1024.0 {
        format!("{b} B")
    } else if b < 1024.0 * 1024.0 {
        format!("{:.0} KB", b / 1024.0)
    } else if b < 1024.0 * 1024.0 * 1024.0 {
        format!("{:.1} MB", b / 1048576.0).replace('.', ",")
    } else {
        format!("{:.1} GB", b / 1073741824.0).replace('.', ",")
    }
}

fn date(ms: i64) -> String {
    // Tag.Monat.Jahr aus Unix-Millisekunden (UTC reicht für die Anzeige).
    let days = ms.div_euclid(86_400_000);
    let (y, m, d) = civil_from_days(days);
    format!("{d:02}.{m:02}.{y}")
}

fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

fn ext(name: &str) -> String {
    name.rsplit('.').next().unwrap_or("").to_ascii_lowercase()
}

fn is_image(name: &str) -> bool {
    matches!(ext(name).as_str(), "jpg" | "jpeg" | "png" | "gif" | "webp")
}

fn is_viewable(name: &str) -> bool {
    is_image(name) || matches!(ext(name).as_str(), "pdf" | "txt" | "mp4" | "webm" | "mp3" | "wav" | "ogg")
}

const ICON_CUBE: &str = r##"<svg width="22" height="22" viewBox="0 0 20 20" fill="none" stroke="#19e3f0" stroke-width="1.6" stroke-linejoin="round" aria-hidden="true"><path d="M10 2.5l6.5 3.75v7.5L10 17.5l-6.5-3.75v-7.5z"/><path d="M3.5 6.25L10 10l6.5-3.75M10 10v7.5"/></svg>"##;
const ICON_FOLDER: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="#0c5c64" stroke="#19e3f0" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/></svg>"##;
const ICON_FILE: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="#95a2b0" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><path d="M4 1.5h5l3 3v10H4z"/><path d="M9 1.5v3h3"/></svg>"##;
const ICON_IMAGE: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="#a084ff" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="3" width="12" height="10" rx="1"/><circle cx="6" cy="6.5" r="1"/><path d="M2.5 12l3.5-3.5 2.5 2.5 2-2 3 3"/></svg>"##;
const ICON_DOWNLOAD: &str = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 2.5v8M5 7.5l3 3 3-3"/><path d="M2.5 10.5v3h11v-3"/></svg>"##;

const CSS: &str = r#"
*{box-sizing:border-box}
body{margin:0;background:#0b0e12;color:#e6ecf2;font:15px/1.45 "Segoe UI",Ubuntu,system-ui,sans-serif}
header{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:12px 20px;border-bottom:1px solid #28303a;background:#0b0e12}
.brand{display:flex;align-items:center;gap:8px;font-weight:600;font-size:12px;letter-spacing:.06em}
.tag{color:#95a2b0;font-size:13px}
main{max-width:880px;margin:0 auto;padding:24px 16px 48px}
h1{font-weight:400;font-size:26px;margin:4px 0 16px;overflow-wrap:anywhere}
.crumbs{color:#95a2b0;font-size:14px;overflow-wrap:anywhere}
.crumbs a{color:#95a2b0;text-decoration:none}.crumbs a:hover{color:#19e3f0}
.sep{margin:0 6px;color:#55606c}
.bar{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:12px;margin-bottom:12px}
.meta{color:#95a2b0;font-size:13px;white-space:nowrap}
.list{list-style:none;margin:0;padding:0;border:1px solid #28303a;border-radius:8px;overflow:hidden;background:#12161c}
.list li+li{border-top:1px solid #1a2028}
.row{display:flex;align-items:center;gap:12px;min-height:52px;padding:6px 12px;color:#e6ecf2;text-decoration:none}
a.row:hover{background:#1a2028}
.ic{width:36px;height:36px;flex:0 0 auto;display:flex;align-items:center;justify-content:center;border-radius:4px;background:#1a2028}
.ic.dir{background:#0b0e12}
.name{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.btn{display:inline-flex;align-items:center;gap:6px;height:36px;min-width:36px;justify-content:center;padding:0 12px;border:1px solid #28303a;border-radius:4px;background:#1a2028;color:#e6ecf2;text-decoration:none;font-size:12px;font-weight:600;letter-spacing:.04em;text-transform:uppercase}
.btn:hover{background:#232b35;border-color:#0c5c64}
.btn.primary{background:#19e3f0;border-color:#19e3f0;color:#0b0e12}.btn.primary:hover{background:#6ceef6}
.single{display:flex;flex-direction:column;align-items:center;gap:8px;padding:40px 16px;border:1px solid #28303a;border-radius:8px;background:#12161c;text-align:center}
.single .name{white-space:normal;font-size:18px}
.ic.big{width:72px;height:72px}.ic.big svg{width:40px;height:40px}
.actions{display:flex;gap:8px;margin-top:8px}
.empty{color:#95a2b0}
footer{text-align:center;color:#55606c;font-size:12px;padding:16px}
@media (max-width:560px){.row .meta{display:none}h1{font-size:22px}}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping() {
        assert_eq!(esc("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        assert_eq!(url_q("Fotos/Ür laub&x"), "Fotos/%C3%9Cr%20laub%26x");
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "01.01.1970");
        assert_eq!(date(1_794_425_083_326), "11.11.2026");
    }
}
