//! Freigabe-Links: Ein Ordner (oder eine Datei) wird über eine öffentliche
//! Webseite /s/<token> lesbar – optional mit Upload für Gäste, Passwort und
//! Ablaufdatum. Dazu die Startseite unter „/“.

use std::io::Write;

use axum::{
    body::Body,
    extract::{Multipart, Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{Html, IntoResponse, Redirect, Response},
    Form, Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    auth::{self, AuthUser},
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

const SHARE_COLS: &str = "token, path, created_at, allow_upload, password_hash IS NOT NULL AS has_password, expires_at, notify";

#[derive(Serialize, sqlx::FromRow)]
pub struct Share {
    pub token: String,
    pub path: String,
    pub created_at: i64,
    pub allow_upload: bool,
    pub has_password: bool,
    pub expires_at: Option<i64>,
    pub notify: bool,
}

#[derive(Deserialize, Default)]
pub struct Settings {
    allow_upload: Option<bool>,
    /// Neues Passwort; leerer Text entfernt das Passwort.
    password: Option<String>,
    /// Ablauf (Unix-ms); 0 entfernt das Ablaufdatum.
    expires_at: Option<i64>,
    notify: Option<bool>,
}

#[derive(Deserialize)]
pub struct CreateReq {
    path: String,
    #[serde(flatten)]
    settings: Settings,
}

async fn get_share(state: &AppState, user_id: i64, token: &str) -> ApiResult<Share> {
    sqlx::query_as(&format!("SELECT {SHARE_COLS} FROM shares WHERE token = ? AND user_id = ?"))
        .bind(token)
        .bind(user_id)
        .fetch_optional(&state.db)
        .await?
        .ok_or(ApiError::NotFound)
}

async fn apply_settings(state: &AppState, token: &str, s: &Settings) -> ApiResult<()> {
    if let Some(v) = s.allow_upload {
        sqlx::query("UPDATE shares SET allow_upload = ? WHERE token = ?").bind(v).bind(token).execute(&state.db).await?;
    }
    if let Some(v) = s.notify {
        sqlx::query("UPDATE shares SET notify = ? WHERE token = ?").bind(v).bind(token).execute(&state.db).await?;
    }
    if let Some(v) = s.expires_at {
        let v = (v > 0).then_some(v);
        sqlx::query("UPDATE shares SET expires_at = ? WHERE token = ?").bind(v).bind(token).execute(&state.db).await?;
    }
    if let Some(pw) = &s.password {
        let hash = if pw.is_empty() {
            None
        } else {
            if pw.len() < 4 {
                return Err(ApiError::BadRequest("Passwort: mindestens 4 Zeichen".into()));
            }
            Some(auth::hash_password(pw)?)
        };
        sqlx::query("UPDATE shares SET password_hash = ? WHERE token = ?").bind(hash).bind(token).execute(&state.db).await?;
    }
    Ok(())
}

/// Legt einen Link an (oder gibt den bestehenden für diesen Pfad zurück).
pub async fn create(State(state): State<AppState>, user: AuthUser, Json(req): Json<CreateReq>) -> ApiResult<Json<Share>> {
    let path = paths::normalize_non_root(&req.path)?;
    let cfg = crate::settings::load(&state).await;
    if !cfg.links_enabled {
        return Err(ApiError::BadRequest("Share links are turned off on this server / Freigabe-Links sind auf diesem Server ausgeschaltet".into()));
    }
    if files::get_live_pool(&state, user.id, &path).await?.is_none() {
        return Err(ApiError::NotFound);
    }
    let existing: Option<(String,)> = sqlx::query_as("SELECT token FROM shares WHERE user_id = ? AND path = ?")
        .bind(user.id)
        .bind(&path)
        .fetch_optional(&state.db)
        .await?;
    let token = match existing {
        Some((t,)) => t,
        None => {
            let mut raw = [0u8; 12];
            rand::rngs::OsRng.fill_bytes(&mut raw);
            let token = hex::encode(raw);
            // Standard-Ablauf aus den Server-Einstellungen
            let expires = (cfg.link_expiry_days > 0).then(|| now_ms() + cfg.link_expiry_days as i64 * 86_400_000);
            sqlx::query("INSERT INTO shares (token, user_id, path, created_at, expires_at) VALUES (?, ?, ?, ?, ?)")
                .bind(&token)
                .bind(user.id)
                .bind(&path)
                .bind(now_ms())
                .bind(expires)
                .execute(&state.db)
                .await?;
            token
        }
    };
    apply_settings(&state, &token, &req.settings).await?;
    Ok(Json(get_share(&state, user.id, &token).await?))
}

pub async fn update(
    State(state): State<AppState>,
    user: AuthUser,
    Path(token): Path<String>,
    Json(req): Json<Settings>,
) -> ApiResult<Json<Share>> {
    get_share(&state, user.id, &token).await?;
    apply_settings(&state, &token, &req).await?;
    Ok(Json(get_share(&state, user.id, &token).await?))
}

pub async fn list(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<Share>>> {
    Ok(Json(
        sqlx::query_as(&format!("SELECT {SHARE_COLS} FROM shares WHERE user_id = ? ORDER BY created_at DESC"))
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
    #[serde(default)]
    ok: Option<usize>,
}

struct Resolved {
    user_id: i64,
    token: String,
    /// Freigegebener Pfad (Wurzel der Freigabe)
    root: String,
    /// Pfad relativ zur Freigabe
    rel: String,
    entry: Entry,
    allow_upload: bool,
    expires_at: Option<i64>,
}

fn cookie_name(token: &str) -> String {
    format!("dcs_{token}")
}

fn cookie_value(secret: &str, token: &str, pw_hash: &str) -> String {
    hex::encode(Sha256::digest(format!("{secret}:{token}:{pw_hash}").as_bytes()))
}

fn has_cookie(headers: &HeaderMap, name: &str, value: &str) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .any(|(k, v)| k == name && v == value)
}

async fn resolve(state: &AppState, token: &str, rel: &str, headers: &HeaderMap) -> Result<Resolved, Response> {
    type Row = (i64, String, bool, Option<String>, Option<i64>);
    let row: Option<Row> = sqlx::query_as("SELECT user_id, path, allow_upload, password_hash, expires_at FROM shares WHERE token = ?")
        .bind(token)
        .fetch_optional(&state.db)
        .await
        .map_err(|e| ApiError::from(e).into_response())?;
    let Some((user_id, root, allow_upload, pw_hash, expires_at)) = row else { return Err(not_found_page()) };
    let cfg = crate::settings::load(state).await;
    if !cfg.links_enabled {
        return Err(not_found_page());
    }
    let allow_upload = allow_upload && cfg.guest_upload;
    if expires_at.map(|t| t < now_ms()).unwrap_or(false) {
        return Err(message_page(StatusCode::GONE, "Link abgelaufen", "Diese Freigabe ist nicht mehr gültig."));
    }
    if let Some(h) = &pw_hash {
        if !has_cookie(headers, &cookie_name(token), &cookie_value(&state.secret, token, h)) {
            return Err(password_page(token, false));
        }
    }
    let rel = paths::normalize(rel).map_err(|_| not_found_page())?;
    let full = if rel.is_empty() { root.clone() } else { format!("{root}/{rel}") };
    let entry = files::get_live_pool(state, user_id, &full)
        .await
        .map_err(|e| e.into_response())?
        .ok_or_else(not_found_page)?;
    Ok(Resolved { user_id, token: token.to_string(), root, rel, entry, allow_upload, expires_at })
}

#[derive(Deserialize)]
pub struct AuthForm {
    password: String,
}

/// Passwort-Eingabe für geschützte Links: setzt ein Cookie für 30 Tage.
pub async fn auth(State(state): State<AppState>, Path(token): Path<String>, Form(f): Form<AuthForm>) -> Response {
    let row: Option<(Option<String>,)> = sqlx::query_as("SELECT password_hash FROM shares WHERE token = ?")
        .bind(&token)
        .fetch_optional(&state.db)
        .await
        .unwrap_or(None);
    let Some((Some(hash),)) = row else { return Redirect::to(&format!("/s/{token}")).into_response() };
    let pw = f.password.clone();
    let h2 = hash.clone();
    let ok = tokio::task::spawn_blocking(move || auth::verify_password(&pw, &h2)).await.unwrap_or(false);
    if !ok {
        tokio::time::sleep(std::time::Duration::from_millis(800)).await;
        return password_page(&token, true);
    }
    let cookie = format!(
        "{}={}; Path=/s/{token}; Max-Age=2592000; HttpOnly; SameSite=Lax; Secure",
        cookie_name(&token),
        cookie_value(&state.secret, &token, &hash)
    );
    let mut resp = Redirect::to(&format!("/s/{token}")).into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        resp.headers_mut().insert(header::SET_COOKIE, v);
    }
    resp
}

/// Upload durch Gäste (nur wenn für den Link erlaubt).
pub async fn upload(
    State(state): State<AppState>,
    Path(token): Path<String>,
    Query(q): Query<PubQuery>,
    headers: HeaderMap,
    mut mp: Multipart,
) -> Response {
    let r = match resolve(&state, &token, &q.p, &headers).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if !r.allow_upload || !r.entry.is_dir {
        return message_page(StatusCode::FORBIDDEN, "Upload nicht erlaubt", "Für diesen Link sind Uploads ausgeschaltet.");
    }
    let mut guest = String::new();
    let device = device_label(headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or(""));
    let mut count = 0usize;
    loop {
        let field = match mp.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => return message_page(StatusCode::BAD_REQUEST, "Upload abgebrochen", &e.to_string()),
        };
        match field.name() {
            Some("name") => {
                guest = field.text().await.unwrap_or_default().trim().chars().take(40).collect();
            }
            Some("files") => {
                let Some(fname) = field.file_name().map(|n| n.to_string()) else { continue };
                // Nur den Dateinamen nehmen, keine Pfade aus dem Browser.
                let fname = fname.rsplit(['/', '\\']).next().unwrap_or("").trim().to_string();
                if fname.is_empty() {
                    continue;
                }
                let target = match free_name(&state, r.user_id, &r.entry.path, &fname).await {
                    Ok(t) => t,
                    Err(e) => return e.into_response(),
                };
                let by = if guest.is_empty() { format!("Gast · {device}") } else { format!("Gast: {guest}") };
                let res = files::store_file(&state, r.user_id, target, Box::pin(field), None, now_ms(), Some(0), &by).await;
                match res {
                    Ok(_) => count += 1,
                    Err(ApiError::QuotaExceeded) => {
                        return message_page(StatusCode::INSUFFICIENT_STORAGE, "Kein Platz mehr", "Der Speicher für diese Freigabe ist voll.")
                    }
                    Err(e) => return e.into_response(),
                }
            }
            _ => {}
        }
    }
    let mut url = format!("/s/{}?ok={count}", r.token);
    if !r.rel.is_empty() {
        url.push_str(&format!("&p={}", url_q(&r.rel)));
    }
    Redirect::to(&url).into_response()
}

/// Freier Dateiname im Ordner: "Foto.jpg", sonst "Foto (2).jpg", …
async fn free_name(state: &AppState, user_id: i64, dir: &str, name: &str) -> ApiResult<String> {
    let full = |n: &str| paths::normalize_non_root(&format!("{dir}/{n}"));
    let first = full(name)?;
    if files::get_live_pool(state, user_id, &first).await?.is_none() {
        return Ok(first);
    }
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    for n in 2..1000 {
        let cand = full(&format!("{stem} ({n}){ext}"))?;
        if files::get_live_pool(state, user_id, &cand).await?.is_none() {
            return Ok(cand);
        }
    }
    Err(ApiError::Conflict("Kein freier Dateiname".into()))
}

fn rel_of(r: &Resolved, name: &str) -> String {
    if r.rel.is_empty() {
        name.to_string()
    } else {
        format!("{}/{}", r.rel, name)
    }
}

pub async fn page(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>, headers: HeaderMap) -> Response {
    let r = match resolve(&state, &token, &q.p, &headers).await {
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
    if let Some(n) = q.ok {
        body.push_str(&format!(
            r#"<p class="notice">{} hochgeladen. Danke!</p>"#,
            if n == 1 { "1 Datei".to_string() } else { format!("{n} Dateien") }
        ));
    }
    if r.entry.is_dir {
        let items = match files::children(&state, r.user_id, &r.entry.path).await {
            Ok(i) => i,
            Err(e) => return e.into_response(),
        };
        let files_count = items.iter().filter(|e| !e.is_dir).count();
        let total: i64 = items.iter().map(|e| e.size).sum();
        body.push_str(&format!(
            r#"<div class="bar"><span class="meta">{} Ordner · {} Dateien · {}</span><a class="btn primary" href="{base}/zip?p={}">{ICON_DOWNLOAD}Alles herunterladen</a></div>"#,
            items.len() - files_count,
            files_count,
            human(total),
            url_q(&r.rel)
        ));

        if r.allow_upload {
            body.push_str(&format!(
                r#"<form class="drop" id="drop" method="post" enctype="multipart/form-data" action="{base}/upload?p={}">
<input id="pick" class="pick" type="file" name="files" multiple required>
<label for="pick" class="drop-area">{ICON_UPLOAD_BIG}<span class="drop-main"><span class="desk">Dateien hierher ziehen</span><span class="touch">Fotos oder Dateien hochladen</span></span><span class="drop-sub"><span class="desk">oder <u>auswählen</u></span><span class="touch"><u>Tippen zum Auswählen</u></span></span></label>
<div class="drop-progress" hidden><div class="drop-line"><span class="drop-status">Wird hochgeladen …</span><span class="drop-pct">0 %</span></div><div class="track"><div class="fill"></div></div></div>
<noscript><button class="btn primary" type="submit">Hochladen</button></noscript>
</form><script src="/assets/upload.js" defer></script>"#,
                url_q(&r.rel)
            ));
        }

        if items.is_empty() {
            body.push_str(r#"<p class="empty">Dieser Ordner ist leer.</p>"#);
        }
        body.push_str(r#"<ul class="list">"#);
        for e in &items {
            let name = name_of(e);
            let rel = url_q(&rel_of(&r, name));
            if e.is_dir {
                body.push_str(&format!(
                    r#"<li><a class="row" href="{base}?p={rel}"><span class="ic dir">{ICON_FOLDER}</span><span class="txt"><span class="name">{}</span><span class="meta">Ordner</span></span></a></li>"#,
                    esc(name)
                ));
            } else {
                let view = if is_viewable(name) {
                    format!(r#"<a class="btn icon" href="{base}/view?p={rel}" aria-label="{} ansehen" title="Ansehen">{ICON_EYE}</a>"#, esc(name))
                } else {
                    String::new()
                };
                let ic = if is_image(name) {
                    format!(r#"<img src="{base}/file?p={rel}&amp;inline=1" alt="" loading="lazy">"#)
                } else {
                    icon_for(name).to_string()
                };
                body.push_str(&format!(
                    r#"<li><div class="row"><span class="ic">{}</span><span class="txt"><span class="name">{}</span><span class="meta">{} · {}</span></span>{view}<a class="btn icon" href="{base}/file?p={rel}" aria-label="{} herunterladen" title="Herunterladen">{ICON_DOWNLOAD}</a></div></li>"#,
                    ic,
                    esc(name),
                    human(e.size),
                    date(e.modified_at),
                    esc(name)
                ));
            }
        }
        body.push_str("</ul>");
    } else {
        let name = name_of(&r.entry);
        let preview = media_html(&base, &url_q(&r.rel), name, "single-media") + &wave_html(&base, &url_q(&r.rel), name, r.entry.size);
        body.push_str(&format!(
            r#"<div class="single">{}<div class="name">{}</div><div class="meta">{} · {}</div><div class="actions"><a class="btn primary" href="{base}/file?p={}">{ICON_DOWNLOAD}Herunterladen</a></div></div>"#,
            if preview.is_empty() { format!(r#"<span class="ic big">{}</span>"#, icon_for(name)) } else { preview },
            esc(name),
            human(r.entry.size),
            date(r.entry.modified_at),
            url_q(&r.rel)
        ));
    }
    page_html(&esc(&title), &format!(r#"<nav class="crumbs">{crumbs}</nav><h1>{}</h1>{}{body}"#, esc(&title), expiry_html(r.expires_at)))
}

/// Vorschau-Seite für ein Bild/Video/Audio mit Vor/Zurück.
pub async fn view(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>, headers: HeaderMap) -> Response {
    let r = match resolve(&state, &token, &q.p, &headers).await {
        Ok(r) => r,
        Err(resp) => return resp,
    };
    if r.entry.is_dir {
        return Redirect::to(&format!("/s/{token}?p={}", url_q(&r.rel))).into_response();
    }
    let base = format!("/s/{token}");
    let name = name_of(&r.entry).to_string();
    let parent_rel = r.rel.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
    let parent_full = r.entry.path.rsplit_once('/').map(|(p, _)| p.to_string()).unwrap_or_default();
    let siblings: Vec<String> = files::children(&state, r.user_id, &parent_full)
        .await
        .unwrap_or_default()
        .iter()
        .filter(|e| !e.is_dir && is_viewable(name_of(e)))
        .map(|e| name_of(e).to_string())
        .collect();
    let pos = siblings.iter().position(|n| *n == name);
    let link = |n: &str| {
        let rel = if parent_rel.is_empty() { n.to_string() } else { format!("{parent_rel}/{n}") };
        format!("{base}/view?p={}", url_q(&rel))
    };
    let prev = pos.and_then(|i| i.checked_sub(1)).map(|i| link(&siblings[i]));
    let next = pos.and_then(|i| siblings.get(i + 1)).map(|n| link(n));
    let nav = |href: Option<String>, label: &str| match href {
        Some(h) => format!(r#"<a class="btn" href="{h}">{label}</a>"#),
        None => format!(r#"<span class="btn disabled">{label}</span>"#),
    };
    let rel = url_q(&r.rel);
    let back = if parent_rel.is_empty() { base.clone() } else { format!("{base}?p={}", url_q(&parent_rel)) };
    let media = media_html(&base, &rel, &name, "viewer-media");
    let media = if media.is_empty() {
        format!(r#"<a class="btn" href="{base}/file?p={rel}&amp;inline=1" target="_blank" rel="noopener">Im Browser öffnen</a>"#)
    } else {
        media
    };
    let body = format!(
        r#"<nav class="crumbs"><a href="{back}">Zurück zum Ordner</a></nav><h1>{}</h1>{}<div class="viewer">{media}</div>{}
<div class="viewer-bar">{}<span class="meta">{} · {}{}</span>{}<a class="btn primary" href="{base}/file?p={rel}">{ICON_DOWNLOAD}Herunterladen</a></div>"#,
        esc(&name),
        expiry_html(r.expires_at),
        wave_html(&base, &rel, &name, r.entry.size),
        nav(prev, "Zurück"),
        human(r.entry.size),
        date(r.entry.modified_at),
        pos.map(|i| format!(" · {} von {}", i + 1, siblings.len())).unwrap_or_default(),
        nav(next, "Weiter"),
    );
    page_html(&esc(&name), &body)
}

fn media_html(base: &str, rel: &str, name: &str, class: &str) -> String {
    let src = format!("{base}/file?p={rel}&amp;inline=1");
    if is_image(name) {
        format!(r#"<img class="{class}" src="{src}" alt="{}">"#, esc(name))
    } else if is_video(name) {
        format!(r#"<video class="{class}" src="{src}" controls playsinline preload="metadata"></video>"#)
    } else if is_audio(name) {
        format!(r#"<audio class="{class}" src="{src}" controls preload="metadata"></audio>"#)
    } else {
        String::new()
    }
}

/// Wellenform unter Video/Audio (wird im Browser aus der Tonspur berechnet).
fn wave_html(base: &str, rel: &str, name: &str, size: i64) -> String {
    if !(is_video(name) || is_audio(name)) {
        return String::new();
    }
    format!(
        r#"<div class="wave" data-src="{base}/file?p={rel}&amp;inline=1" data-size="{size}"><canvas></canvas><div class="wave-status">Wellenform wird berechnet …</div></div>
<div class="times"><span class="t-pos">00:00.0</span><span class="t-rem">-00:00.0</span></div><script src="/assets/player.js" defer></script>"#
    )
}

fn expiry_html(expires_at: Option<i64>) -> String {
    match expires_at {
        Some(t) => format!(r#"<p class="expires">{ICON_CLOCK}Link gültig bis {}</p>"#, date_time(t)),
        None => String::new(),
    }
}

fn name_of(e: &Entry) -> &str {
    e.path.rsplit('/').next().unwrap_or("")
}

fn icon_for(name: &str) -> &'static str {
    if is_image(name) {
        ICON_IMAGE
    } else if is_video(name) || is_audio(name) {
        ICON_MEDIA
    } else {
        ICON_FILE
    }
}

pub async fn file(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>, headers: HeaderMap) -> Response {
    let r = match resolve(&state, &token, &q.p, &headers).await {
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

// ------------------------------------------------------------ Startseite

fn app_exe(state: &AppState) -> std::path::PathBuf {
    let public = state.cfg.data_dir.join("public");
    let current = public.join("DropCraft-Windows.exe");
    if current.exists() {
        current
    } else {
        public.join("DropCraft.exe")
    }
}

pub async fn landing(State(state): State<AppState>) -> Response {
    let download = match tokio::fs::metadata(app_exe(&state)).await {
        Ok(m) => format!(
            r#"<a class="btn big" href="/download/windows">{ICON_DOWNLOAD}Windows-App herunterladen</a><span class="meta">DropCraft.exe · {}</span>"#,
            human(m.len() as i64)
        ),
        Err(_) => String::new(),
    };
    let mac = state.cfg.data_dir.join("public").join("DropCraft-macOS.zip");
    let download = if mac.exists() {
        format!(r#"{download}<a class="btn big" href="/updates/DropCraft-macOS.zip">{ICON_DOWNLOAD}Mac-App herunterladen</a>"#)
    } else {
        download
    };
    let site = esc(&crate::settings::load(&state).await.site_name);
    let body = format!(
        r#"<section class="hero"><div class="hero-logo">{ICON_LOGO_BIG}</div><h1>{site}</h1>
<p class="lead">Private Cloud zum Synchronisieren und Teilen von Dateien.</p>
<div class="hero-actions"><a class="btn primary big" href="/app/">Im Browser öffnen</a>{download}</div>
<p class="meta">Du hast einen Link bekommen? Öffne ihn einfach direkt – dafür brauchst du nichts zu installieren.</p>
<p class="meta"><a class="link-plain" href="/account">Konto verwalten · Manage account</a></p></section>"#
    );
    page_html("DropCraft", &body)
}

pub async fn download_app(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let path = app_exe(&state);
    let Ok(meta) = tokio::fs::metadata(&path).await else { return not_found_page() };
    let file = match tokio::fs::File::open(&path).await {
        Ok(f) => f,
        Err(_) => return not_found_page(),
    };
    let _ = headers;
    (
        [
            (header::CONTENT_TYPE, "application/vnd.microsoft.portable-executable".to_string()),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"DropCraft.exe\"".to_string()),
            (header::CONTENT_LENGTH, meta.len().to_string()),
        ],
        Body::from_stream(tokio_util::io::ReaderStream::new(file)),
    )
        .into_response()
}

/// Ganzer Ordner als ZIP (ohne Kompression, gestreamt).
pub async fn zip(State(state): State<AppState>, Path(token): Path<String>, Query(q): Query<PubQuery>, headers: HeaderMap) -> Response {
    let r = match resolve(&state, &token, &q.p, &headers).await {
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
    message_page(StatusCode::NOT_FOUND, "Link nicht gefunden", "Diese Freigabe gibt es nicht mehr oder der Link ist falsch.")
}

fn message_page(status: StatusCode, title: &str, text: &str) -> Response {
    let mut r = page_html(&esc(title), &format!(r#"<h1>{}</h1><p class="empty">{}</p>"#, esc(title), esc(text)));
    *r.status_mut() = status;
    r
}

fn password_page(token: &str, wrong: bool) -> Response {
    let hint = if wrong { r#"<p class="error">Falsches Passwort.</p>"# } else { "" };
    let mut r = page_html(
        "Passwort erforderlich",
        &format!(
            r#"<section class="hero"><div class="hero-logo">{ICON_LOCK}</div><h1>Passwort erforderlich</h1>{hint}
<form class="pw" method="post" action="/s/{}/auth"><label class="field"><span>Passwort</span><input type="password" name="password" autofocus required></label>
<button class="btn primary" type="submit">Öffnen</button></form></section>"#,
            esc(token)
        ),
    );
    *r.status_mut() = StatusCode::UNAUTHORIZED;
    r
}

fn page_html(title: &str, main: &str) -> Response {
    let html = format!(
        r#"<!doctype html><html lang="de"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1,viewport-fit=cover">
<meta name="robots" content="noindex"><link rel="icon" href="/app/icon-192.png"><title>{title} · DropCraft</title><style>{CSS}</style></head>
<body><header><a class="brand" href="/">{ICON_LOGO}<span>DROPCRAFT</span></a></header>
<main>{main}</main></body></html>"#
    );
    let mut resp = Html(html).into_response();
    let h = resp.headers_mut();
    h.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(
            "default-src 'none'; script-src 'self'; connect-src 'self'; style-src 'unsafe-inline'; img-src 'self' data:; media-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
        ),
    );
    h.insert(header::REFERRER_POLICY, HeaderValue::from_static("no-referrer"));
    h.insert(header::X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    resp
}

/// Skript für Ziehen & Ablegen und Fortschritt (eigene Datei wegen CSP).
const UPLOAD_JS: &str = r#"(function(){
var f=document.getElementById('drop');if(!f)return;
var pick=document.getElementById('pick'),prog=f.querySelector('.drop-progress'),st=f.querySelector('.drop-status'),pct=f.querySelector('.drop-pct'),fill=f.querySelector('.fill');
function send(files){
 if(!files||!files.length)return;
 var d=new FormData();for(var i=0;i<files.length;i++)d.append('files',files[i],files[i].name);
 f.classList.add('busy');prog.hidden=false;
 st.textContent=files.length==1?files[0].name:files.length+' Dateien';
 var x=new XMLHttpRequest();x.open('POST',f.action);
 x.upload.onprogress=function(e){if(e.lengthComputable){var p=Math.floor(e.loaded*100/e.total);fill.style.width=p+'%';pct.textContent=p+' %';}};
 x.onload=function(){if(x.status<400){fill.style.width='100%';pct.textContent='100 %';location.href=x.responseURL||location.href;}else fail(x.status==507?'Kein Platz mehr frei':'Upload fehlgeschlagen');};
 x.onerror=function(){fail('Verbindung abgebrochen');};
 x.send(d);
}
function fail(m){f.classList.add('failed');st.textContent=m;pct.textContent='';setTimeout(function(){f.classList.remove('busy','failed');prog.hidden=true;fill.style.width='0';},3500);}
pick.addEventListener('change',function(){send(pick.files);});
var n=0;
document.addEventListener('dragenter',function(e){e.preventDefault();n++;f.classList.add('over');});
document.addEventListener('dragleave',function(){if(--n<=0){n=0;f.classList.remove('over');}});
document.addEventListener('dragover',function(e){e.preventDefault();});
document.addEventListener('drop',function(e){e.preventDefault();n=0;f.classList.remove('over');if(e.dataTransfer)send(e.dataTransfer.files);});
})();"#;

/// Wellenform-Player (Stil wie DMXCraft): Balken, gespielter Teil in Cyan,
/// weiße Abspielmarke, Klicken/Ziehen springt, Zeit und Restzeit darunter.
const PLAYER_JS: &str = r#"(function(){
var w=document.querySelector('.wave');if(!w)return;
var m=document.querySelector('video.viewer-media,audio.viewer-media,video.single-media,audio.single-media');if(!m)return;
var c=w.querySelector('canvas'),st=w.querySelector('.wave-status'),tp=document.querySelector('.t-pos'),tr=document.querySelector('.t-rem');
var peaks=null,ctx=c.getContext('2d');
function clock(s){if(!isFinite(s)||s<0)s=0;var ms=Math.floor(s*1000);var mm=Math.floor(ms/60000),ss=Math.floor(ms/1000)%60,t=Math.floor(ms/100)%10;return(mm<10?'0':'')+mm+':'+(ss<10?'0':'')+ss+'.'+t;}
function draw(){
 var dpr=window.devicePixelRatio||1,W=c.clientWidth,H=c.clientHeight;
 if(c.width!==Math.round(W*dpr)||c.height!==Math.round(H*dpr)){c.width=Math.round(W*dpr);c.height=Math.round(H*dpr);}
 ctx.setTransform(dpr,0,0,dpr,0,0);ctx.clearRect(0,0,W,H);
 var d=m.duration,f=(d>0&&isFinite(d))?m.currentTime/d:0;
 if(peaks){
  var bars=Math.max(1,Math.floor((W-4)/3)),n=peaks.length;
  for(var b=0;b<bars;b++){
   var s0=Math.floor(b*n/bars),e0=Math.min(n,Math.max(s0+1,Math.floor((b+1)*n/bars))),pk=0;
   for(var i=s0;i<e0;i++)if(peaks[i]>pk)pk=peaks[i];
   var h=Math.max(2,pk*(H-8)),x=2+b*3;
   ctx.fillStyle=(b/bars)<f?'#19e3f0':'#2b3542';
   ctx.fillRect(x,(H-h)/2,2,h);
  }
 }
 if(d>0&&isFinite(d)){ctx.fillStyle='#e6ecf2';ctx.fillRect(Math.min(W-2,Math.max(0,f*W-1)),0,2,H);}
 if(tp){tp.textContent=clock(m.currentTime);tp.classList.toggle('on',!m.paused);}
 if(tr){var r=(d||0)-m.currentTime;tr.textContent='-'+clock(r);tr.classList.toggle('warn',!m.paused&&r<10);}
}
var raf=0;function loop(){draw();if(!m.paused)raf=requestAnimationFrame(loop);}
m.addEventListener('play',function(){cancelAnimationFrame(raf);loop();});
['pause','seeked','timeupdate','loadedmetadata','durationchange'].forEach(function(ev){m.addEventListener(ev,draw);});
window.addEventListener('resize',draw);
var drag=false;
function seek(e){var r=c.getBoundingClientRect(),f=Math.min(1,Math.max(0,(e.clientX-r.left)/r.width));if(m.duration>0&&isFinite(m.duration)){m.currentTime=f*m.duration;draw();}}
w.addEventListener('pointerdown',function(e){drag=true;w.setPointerCapture(e.pointerId);seek(e);});
w.addEventListener('pointermove',function(e){if(drag)seek(e);});
w.addEventListener('pointerup',function(){drag=false;});
draw();
var size=+w.getAttribute('data-size')||0,AC=window.AudioContext||window.webkitAudioContext;
if(!AC){st.textContent='';return;}
if(size>500*1024*1024){st.textContent='Datei zu groß für eine Wellenform';return;}
fetch(w.getAttribute('data-src'),{credentials:'same-origin'}).then(function(r){if(!r.ok)throw 0;return r.arrayBuffer();}).then(function(buf){
 var ac=new AC();
 return new Promise(function(ok,no){ac.decodeAudioData(buf,ok,no);}).then(function(ab){if(ac.close)ac.close();return ab;},function(e){if(ac.close)ac.close();throw e;});
}).then(function(ab){
 var N=2000,len=ab.length,out=new Float32Array(N),chs=[];
 for(var k=0;k<ab.numberOfChannels;k++)chs.push(ab.getChannelData(k));
 var step=Math.max(1,Math.floor(len/N/64)),max=0;
 for(var i=0;i<N;i++){var a=Math.floor(i*len/N),z=Math.floor((i+1)*len/N),p=0;
  for(var q=0;q<chs.length;q++){var ch=chs[q];for(var j=a;j<z;j+=step){var v=ch[j];if(v<0)v=-v;if(v>p)p=v;}}
  out[i]=p;if(p>max)max=p;}
 if(max>0)for(var i2=0;i2<N;i2++)out[i2]/=max;
 peaks=out;st.textContent='';draw();
}).catch(function(){st.textContent='Keine Tonspur gefunden';});
})();"#;

pub async fn player_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        PLAYER_JS,
    )
        .into_response()
}

pub async fn upload_js() -> Response {
    (
        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8"), (header::CACHE_CONTROL, "public, max-age=3600")],
        UPLOAD_JS,
    )
        .into_response()
}

/// Lesbare Gerätebezeichnung aus dem User-Agent, z. B. "iPhone · Safari".
pub fn device_label(ua: &str) -> String {
    let os = if ua.contains("iPhone") {
        "iPhone"
    } else if ua.contains("iPad") || (ua.contains("Macintosh") && ua.contains("Mobile")) {
        "iPad"
    } else if ua.contains("Android") {
        if ua.contains("Mobile") { "Android-Handy" } else { "Android-Tablet" }
    } else if ua.contains("Windows") {
        "Windows-PC"
    } else if ua.contains("Macintosh") || ua.contains("Mac OS X") {
        "Mac"
    } else if ua.contains("CrOS") {
        "Chromebook"
    } else if ua.contains("Linux") {
        "Linux-PC"
    } else {
        ""
    };
    let browser = if ua.contains("Edg/") || ua.contains("EdgA/") || ua.contains("EdgiOS/") {
        "Edge"
    } else if ua.contains("OPR/") || ua.contains("Opera") {
        "Opera"
    } else if ua.contains("SamsungBrowser") {
        "Samsung Internet"
    } else if ua.contains("Firefox/") || ua.contains("FxiOS/") {
        "Firefox"
    } else if ua.contains("Chrome/") || ua.contains("CriOS/") {
        "Chrome"
    } else if ua.contains("Safari/") {
        "Safari"
    } else {
        ""
    };
    match (os, browser) {
        ("", "") => "Browser".to_string(),
        ("", b) => b.to_string(),
        (o, "") => o.to_string(),
        (o, b) => format!("{o} · {b}"),
    }
}

pub(crate) fn esc(s: &str) -> String {
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

/// Größe für die Konto-Website (Deutsch mit Komma).
pub(crate) fn human_l(de: bool, b: i64) -> String {
    let s = human(b);
    if de {
        s
    } else {
        s.replace(',', ".")
    }
}

/// Datum und Uhrzeit (Ortszeit) für die Konto-Website.
pub(crate) fn date_time_l(de: bool, ms: i64) -> String {
    if de {
        return date_time(ms);
    }
    let l = local_ms(ms);
    let (y, m, d) = civil_from_days(l.div_euclid(86_400_000));
    let mins = l.rem_euclid(86_400_000) / 60_000;
    format!("{y}-{m:02}-{d:02} {:02}:{:02}", mins / 60, mins % 60)
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

/// Unix-Millisekunden in deutsche Ortszeit (MEZ/MESZ).
fn local_ms(ms: i64) -> i64 {
    let (y, _, _) = civil_from_days(ms.div_euclid(86_400_000));
    // Sommerzeit: letzter Sonntag im März 01:00 UTC bis letzter Sonntag im Oktober 01:00 UTC.
    let last_sunday = |m: u32| {
        let d = days_from_civil(y, m, 31);
        d - (d + 4).rem_euclid(7)
    };
    let start = last_sunday(3) * 86_400_000 + 3_600_000;
    let end = last_sunday(10) * 86_400_000 + 3_600_000;
    ms + if ms >= start && ms < end { 7_200_000 } else { 3_600_000 }
}

fn date(ms: i64) -> String {
    let (y, m, d) = civil_from_days(local_ms(ms).div_euclid(86_400_000));
    format!("{d:02}.{m:02}.{y}")
}

fn date_time(ms: i64) -> String {
    let l = local_ms(ms);
    let mins = l.rem_euclid(86_400_000) / 60_000;
    format!("{}, {:02}:{:02} Uhr", date(ms), mins / 60, mins % 60)
}

fn days_from_civil(y: i64, m: u32, d: u32) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 } as i64;
    let doy = (153 * mp + 2) / 5 + d as i64 - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
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

fn is_video(name: &str) -> bool {
    matches!(ext(name).as_str(), "mp4" | "webm" | "mov" | "m4v")
}

fn is_audio(name: &str) -> bool {
    matches!(ext(name).as_str(), "mp3" | "wav" | "ogg" | "m4a" | "flac")
}

fn is_viewable(name: &str) -> bool {
    is_image(name) || is_video(name) || is_audio(name) || matches!(ext(name).as_str(), "pdf" | "txt")
}

/// App-Logo (Tropfen mit Sync-Pfeilen), wie im Programmsymbol (client/desktop/src/icons.rs).
pub(crate) const ICON_LOGO: &str = r##"<svg width="24" height="24" viewBox="0 0 64 64" aria-hidden="true"><defs><linearGradient id="dcb" x1="0" y1="0" x2=".35" y2="1"><stop offset="0" stop-color="#1747b8"/><stop offset="1" stop-color="#071538"/></linearGradient><radialGradient id="dcg" cx="61" cy="1" r="22" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#3fd4ff" stop-opacity=".55"/><stop offset="1" stop-color="#3fd4ff" stop-opacity="0"/></radialGradient><linearGradient id="dcd" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5ff4ff"/><stop offset="1" stop-color="#2f7bff"/></linearGradient><linearGradient id="dcv" x1="1" y1="0" x2="0" y2="1"><stop offset=".35" stop-color="#8a5cff" stop-opacity="0"/><stop offset="1" stop-color="#8a5cff" stop-opacity=".6"/></linearGradient><filter id="dcf" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="2.4"/></filter></defs><rect x="1.92" y="1.92" width="60.16" height="60.16" rx="13.24" fill="url(#dcb)"/><rect x="1.92" y="1.92" width="60.16" height="60.16" rx="13.24" fill="url(#dcg)"/><rect x="2.52" y="2.52" width="58.96" height="58.96" rx="12.64" fill="none" stroke="#5aa8ff" stroke-opacity=".4" stroke-width=".8"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="#29d9ff" opacity=".45" filter="url(#dcf)"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="url(#dcd)"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="url(#dcv)"/><path d="M24.48 35.98A8 8 0 0 1 38.93 34.72M39.52 41.46A8 8 0 0 1 25.07 42.72" fill="none" stroke="#fff" stroke-width="2.88" stroke-linecap="round"/><path d="M41.09 38.46L42.36 32.74L35.49 36.7ZM22.91 38.98L21.64 44.7L28.51 40.74Z" fill="#fff"/></svg>"##;
const ICON_FOLDER: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="#0c5c64" stroke="#19e3f0" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><path d="M2 4.5A1.5 1.5 0 0 1 3.5 3h3l1.5 1.5h4.5A1.5 1.5 0 0 1 14 6v5.5a1.5 1.5 0 0 1-1.5 1.5h-9A1.5 1.5 0 0 1 2 11.5z"/></svg>"##;
const ICON_FILE: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="#95a2b0" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><path d="M4 1.5h5l3 3v10H4z"/><path d="M9 1.5v3h3"/></svg>"##;
const ICON_IMAGE: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="#a084ff" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="3" width="12" height="10" rx="1"/><circle cx="6" cy="6.5" r="1"/><path d="M2.5 12l3.5-3.5 2.5 2.5 2-2 3 3"/></svg>"##;
const ICON_EYE: &str = r##"<svg width="18" height="18" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M1.2 8s2.5-4.8 6.8-4.8S14.8 8 14.8 8s-2.5 4.8-6.8 4.8S1.2 8 1.2 8z"/><circle cx="8" cy="8" r="2.2"/></svg>"##;
const ICON_DOWNLOAD: &str = r##"<svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 2.5v8M5 7.5l3 3 3-3"/><path d="M2.5 10.5v3h11v-3"/></svg>"##;

const ICON_UPLOAD_BIG: &str = r##"<svg width="34" height="34" viewBox="0 0 16 16" fill="none" stroke="#19e3f0" stroke-width="1.1" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M8 10.5V2.5M5 5.5l3-3 3 3"/><path d="M2.5 10.5v3h11v-3"/></svg>"##;
const ICON_CLOCK: &str = r##"<svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" aria-hidden="true"><circle cx="8" cy="8" r="6"/><path d="M8 4.5V8l2.5 1.5"/></svg>"##;
const ICON_MEDIA: &str = r##"<svg width="20" height="20" viewBox="0 0 16 16" fill="none" stroke="#f45db8" stroke-width="1.2" stroke-linejoin="round" aria-hidden="true"><rect x="2" y="3" width="12" height="10" rx="1"/><path d="M6.5 6v4l3.5-2z"/></svg>"##;
const ICON_LOCK: &str = r##"<svg width="44" height="44" viewBox="0 0 16 16" fill="none" stroke="#19e3f0" stroke-width="1.1" stroke-linejoin="round" aria-hidden="true"><rect x="3" y="7" width="10" height="7" rx="1"/><path d="M5 7V5a3 3 0 0 1 6 0v2"/></svg>"##;
pub(crate) const ICON_LOGO_BIG: &str = r##"<svg width="72" height="72" viewBox="0 0 64 64" aria-hidden="true"><defs><linearGradient id="dcb2" x1="0" y1="0" x2=".35" y2="1"><stop offset="0" stop-color="#1747b8"/><stop offset="1" stop-color="#071538"/></linearGradient><radialGradient id="dcg2" cx="61" cy="1" r="22" gradientUnits="userSpaceOnUse"><stop offset="0" stop-color="#3fd4ff" stop-opacity=".55"/><stop offset="1" stop-color="#3fd4ff" stop-opacity="0"/></radialGradient><linearGradient id="dcd2" x1="0" y1="0" x2="0" y2="1"><stop offset="0" stop-color="#5ff4ff"/><stop offset="1" stop-color="#2f7bff"/></linearGradient><linearGradient id="dcv2" x1="1" y1="0" x2="0" y2="1"><stop offset=".35" stop-color="#8a5cff" stop-opacity="0"/><stop offset="1" stop-color="#8a5cff" stop-opacity=".6"/></linearGradient><filter id="dcf2" x="-30%" y="-30%" width="160%" height="160%"><feGaussianBlur stdDeviation="2.4"/></filter></defs><rect x="1.92" y="1.92" width="60.16" height="60.16" rx="13.24" fill="url(#dcb2)"/><rect x="1.92" y="1.92" width="60.16" height="60.16" rx="13.24" fill="url(#dcg2)"/><rect x="2.52" y="2.52" width="58.96" height="58.96" rx="12.64" fill="none" stroke="#5aa8ff" stroke-opacity=".4" stroke-width=".8"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="#29d9ff" opacity=".45" filter="url(#dcf2)"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="url(#dcd2)"/><path d="M30.36 9.88A1.92 1.92 0 0 1 33.64 9.88L45.91 29.86A16.32 16.32 0 1 1 18.09 29.86Z" fill="url(#dcv2)"/><path d="M24.48 35.98A8 8 0 0 1 38.93 34.72M39.52 41.46A8 8 0 0 1 25.07 42.72" fill="none" stroke="#fff" stroke-width="2.88" stroke-linecap="round"/><path d="M41.09 38.46L42.36 32.74L35.49 36.7ZM22.91 38.98L21.64 44.7L28.51 40.74Z" fill="#fff"/></svg>"##;
pub(crate) const CSS: &str = r#"
*{box-sizing:border-box}
body{margin:0;background:#0b0e12;color:#e6ecf2;font:15px/1.45 "Segoe UI",Ubuntu,system-ui,sans-serif}
header{display:flex;align-items:center;justify-content:space-between;gap:12px;padding:12px 20px;border-bottom:1px solid #28303a;background:#0b0e12}
.brand{display:flex;align-items:center;gap:8px;font-weight:600;font-size:12px;letter-spacing:.06em;color:#e6ecf2;text-decoration:none}
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
.ic img{width:100%;height:100%;object-fit:cover;border-radius:4px;display:block}
.expires{display:flex;align-items:center;gap:6px;margin:-8px 0 16px;color:#95a2b0;font-size:13px}
.wave{position:relative;height:96px;margin-top:12px;border-radius:4px;background:#0b0e12;border:1px solid #28303a;cursor:pointer;touch-action:none}
.wave canvas{width:100%;height:100%;display:block}
.wave-status{position:absolute;inset:0;display:flex;align-items:center;justify-content:center;color:#55606c;font-size:13px;pointer-events:none}
.times{display:flex;justify-content:space-between;align-items:baseline;margin-top:6px;font-variant-numeric:tabular-nums}
.t-pos{font-size:26px;color:#95a2b0}.t-pos.on{color:#19e3f0}
.t-rem{font-size:15px;color:#95a2b0}.t-rem.warn{color:#fbbf24}
.single .wave,.single .times{width:100%}
.name{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;white-space:nowrap}
.txt .name{flex:none}
.btn{display:inline-flex;align-items:center;gap:6px;height:36px;min-width:36px;justify-content:center;padding:0 12px;border:1px solid #28303a;border-radius:4px;background:#1a2028;color:#e6ecf2;text-decoration:none;font-size:12px;font-weight:600;letter-spacing:.04em;text-transform:uppercase}
.btn:hover{background:#232b35;border-color:#0c5c64}
.btn.primary{background:#19e3f0;border-color:#19e3f0;color:#0b0e12}.btn.primary:hover{background:#6ceef6}
.single{display:flex;flex-direction:column;align-items:center;gap:8px;padding:40px 16px;border:1px solid #28303a;border-radius:8px;background:#12161c;text-align:center}
.single .name{white-space:normal;font-size:18px}
.ic.big{width:72px;height:72px}.ic.big svg{width:40px;height:40px}
.actions{display:flex;gap:8px;margin-top:8px}
.empty{color:#95a2b0}
.notice{background:#0c5c64;border:1px solid #19e3f0;border-radius:4px;padding:10px 12px}
.error{color:#ef4455}
.drop{position:relative;margin:0 0 16px}
.pick{position:absolute;width:1px;height:1px;opacity:0;pointer-events:none}
.drop-area{display:flex;flex-direction:column;align-items:center;justify-content:center;gap:4px;min-height:128px;padding:20px;border:1.5px dashed #28303a;border-radius:8px;background:#12161c;cursor:pointer;text-align:center;transition:border-color .12s,background .12s}
.drop-area:hover,.drop.over .drop-area,.pick:focus-visible+.drop-area{border-color:#19e3f0;background:#0f1d22}
.drop-main{font-size:16px;font-weight:600;margin-top:6px}
.drop-sub{font-size:13px;color:#95a2b0}.drop-sub u{color:#19e3f0;text-decoration:none}
.drop.busy .drop-area{display:none}
.drop-progress{padding:16px;border:1px solid #28303a;border-radius:8px;background:#12161c}
.drop-line{display:flex;justify-content:space-between;gap:12px;margin-bottom:10px;font-size:14px}
.drop-status{overflow:hidden;text-overflow:ellipsis;white-space:nowrap}.drop-pct{color:#95a2b0;font-variant-numeric:tabular-nums}
.track{height:6px;border-radius:3px;background:#1a2028;overflow:hidden}.fill{height:100%;width:0;background:#19e3f0;transition:width .15s}
.drop.failed .fill{background:#ef4455}
.drop noscript .btn{margin-top:10px}
.viewer{display:flex;justify-content:center;align-items:center;background:#000;border:1px solid #28303a;border-radius:8px;min-height:240px;overflow:hidden}
.viewer-media{max-width:100%;max-height:72vh;display:block}
audio.viewer-media{width:100%;margin:40px 16px}
.viewer-bar{display:flex;flex-wrap:wrap;align-items:center;justify-content:space-between;gap:8px;margin-top:12px}
.btn.disabled{opacity:.35;pointer-events:none}
.single-media{max-width:100%;max-height:60vh;border-radius:6px}
.hero{display:flex;flex-direction:column;align-items:center;text-align:center;gap:12px;padding:56px 0}
.hero .meta{white-space:normal}
.hero h1{font-size:36px;margin:0}
.lead{font-size:18px;color:#95a2b0;margin:0}
.link-plain{color:#95a2b0}.link-plain:hover{color:#19e3f0}
.hero-actions{display:flex;flex-direction:column;align-items:center;gap:8px;margin:16px 0}
.btn.big{height:48px;padding:0 20px;font-size:13px}
.pw{display:flex;flex-direction:column;gap:12px;width:100%;max-width:320px;text-align:left}
.txt{flex:1;min-width:0;display:flex;flex-direction:column;gap:1px}
.txt .meta{font-size:12px;overflow:hidden;text-overflow:ellipsis}
.btn.icon{width:40px;height:40px;padding:0;flex:0 0 auto}
.touch{display:none}
header{padding-top:max(12px,env(safe-area-inset-top));padding-left:max(20px,env(safe-area-inset-left));padding-right:max(20px,env(safe-area-inset-right))}
main{padding-bottom:max(48px,env(safe-area-inset-bottom))}
@media (hover:none){.desk{display:none}.touch{display:inline}a.row:hover{background:none}a.row:active,.btn:active{background:#232b35}}
@media (max-width:560px){
main{padding:16px 12px 40px}
h1{font-size:21px;margin-bottom:12px}
.row{gap:10px;padding:6px 8px 6px 10px;min-height:56px}
.row .btn.icon{width:44px;height:44px}
.bar .meta{flex:1 1 100%}.bar .btn{flex:1 1 100%;height:44px}
.viewer-bar .meta{order:-1;flex:1 1 100%;text-align:center}
.btn{white-space:nowrap}
.drop-area{min-height:104px}
.viewer{min-height:180px;border-radius:6px}
.viewer-media{max-height:58vh}
.viewer-bar .btn{flex:1 1 0;height:44px;padding:0 8px}
.viewer-bar .btn.primary{flex:2 1 0}
.t-pos{font-size:22px}
.hero{padding:32px 0}.hero h1{font-size:30px}
.single{padding:28px 12px}
}
"#;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn devices() {
        let iphone = "Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/18.0 Mobile/15E148 Safari/604.1";
        assert_eq!(device_label(iphone), "iPhone · Safari");
        let win = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Safari/537.36 Edg/140.0";
        assert_eq!(device_label(win), "Windows-PC · Edge");
        let android = "Mozilla/5.0 (Linux; Android 15; Pixel 9) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0 Mobile Safari/537.36";
        assert_eq!(device_label(android), "Android-Handy · Chrome");
        assert_eq!(device_label("Mozilla/5.0 (X11; Linux x86_64; rv:140.0) Gecko/20100101 Firefox/140.0"), "Linux-PC · Firefox");
        assert_eq!(device_label("curl/8.0"), "Browser");
    }

    #[test]
    fn escaping() {
        assert_eq!(esc("<a href=\"x\">&'"), "&lt;a href=&quot;x&quot;&gt;&amp;&#39;");
        assert_eq!(url_q("Fotos/Ür laub&x"), "Fotos/%C3%9Cr%20laub%26x");
    }

    #[test]
    fn local_time() {
        // 1.7.2026 12:00 UTC -> 14:00 MESZ; 1.1.2026 12:00 UTC -> 13:00 MEZ
        assert_eq!(date_time(1_782_907_200_000), "01.07.2026, 14:00 Uhr");
        assert_eq!(date_time(1_767_268_800_000), "01.01.2026, 13:00 Uhr");
        assert_eq!(days_from_civil(1970, 1, 1), 0);
        assert_eq!(civil_from_days(days_from_civil(2026, 11, 11)), (2026, 11, 11));
    }

    #[test]
    fn dates() {
        assert_eq!(date(0), "01.01.1970");
        assert_eq!(date(1_794_425_083_326), "11.11.2026");
    }
}
