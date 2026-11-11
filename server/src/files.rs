use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};
use tokio::io::AsyncWriteExt;
use tokio_util::io::ReaderStream;

use crate::{
    auth::AuthUser,
    db::now_ms,
    error::{ApiError, ApiResult},
    paths, AppState,
};

#[derive(Debug, Clone, Serialize, sqlx::FromRow)]
pub struct Entry {
    pub path: String,
    pub is_dir: bool,
    pub size: i64,
    pub sha256: Option<String>,
    pub modified_at: i64,
    pub rev: i64,
    pub deleted: bool,
    #[serde(skip)]
    pub blob_id: Option<String>,
    /// Gerät oder Gast, das die letzte Änderung gemacht hat.
    pub modified_by: Option<String>,
}

const ENTRY_COLS: &str = "path, is_dir, size, sha256, modified_at, rev, deleted, blob_id, modified_by";

type Tx<'a> = Transaction<'a, Sqlite>;

async fn next_rev(tx: &mut Tx<'_>, user_id: i64) -> ApiResult<i64> {
    let (rev,): (i64,) = sqlx::query_as("UPDATE users SET cursor = cursor + 1 WHERE id = ? RETURNING cursor")
        .bind(user_id)
        .fetch_one(&mut **tx)
        .await?;
    Ok(rev)
}

async fn cursor_now(tx: &mut Tx<'_>, user_id: i64) -> ApiResult<i64> {
    let (c,): (i64,) = sqlx::query_as("SELECT cursor FROM users WHERE id = ?").bind(user_id).fetch_one(&mut **tx).await?;
    Ok(c)
}

/// Alle in dieser Transaktion geänderten Einträge mit dem Urheber versehen.
async fn stamp(tx: &mut Tx<'_>, user_id: i64, since: i64, by: &str) -> ApiResult<()> {
    sqlx::query("UPDATE files SET modified_by = ? WHERE user_id = ? AND rev > ?")
        .bind(by)
        .bind(user_id)
        .bind(since)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

async fn get_live(tx: &mut Tx<'_>, user_id: i64, path: &str) -> ApiResult<Option<Entry>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND path = ? AND deleted = 0"
    ))
    .bind(user_id)
    .bind(path)
    .fetch_optional(&mut **tx)
    .await?)
}

/// Eintrag + alle Nachfahren (nur nicht gelöschte), Eltern vor Kindern.
async fn get_subtree(tx: &mut Tx<'_>, user_id: i64, path: &str) -> ApiResult<Vec<Entry>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND deleted = 0 \
         AND (path = ? OR path LIKE ? ESCAPE '\\') ORDER BY length(path)"
    ))
    .bind(user_id)
    .bind(path)
    .bind(paths::like_prefix(path))
    .fetch_all(&mut **tx)
    .await?)
}

#[allow(clippy::too_many_arguments)]
async fn upsert(
    tx: &mut Tx<'_>,
    user_id: i64,
    path: &str,
    is_dir: bool,
    size: i64,
    sha256: Option<&str>,
    blob_id: Option<&str>,
    modified_at: i64,
    rev: i64,
) -> ApiResult<()> {
    sqlx::query(
        "INSERT INTO files (user_id, path, is_dir, size, sha256, blob_id, modified_at, rev, deleted) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0) \
         ON CONFLICT (user_id, path) DO UPDATE SET is_dir = excluded.is_dir, size = excluded.size, \
         sha256 = excluded.sha256, blob_id = excluded.blob_id, modified_at = excluded.modified_at, \
         rev = excluded.rev, deleted = 0",
    )
    .bind(user_id)
    .bind(path)
    .bind(is_dir)
    .bind(size)
    .bind(sha256)
    .bind(blob_id)
    .bind(modified_at)
    .bind(rev)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn tombstone(tx: &mut Tx<'_>, user_id: i64, path: &str, rev: i64) -> ApiResult<()> {
    sqlx::query(
        "UPDATE files SET deleted = 1, blob_id = NULL, sha256 = NULL, size = 0, rev = ? \
         WHERE user_id = ? AND path = ?",
    )
    .bind(rev)
    .bind(user_id)
    .bind(path)
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// Legt fehlende Elternordner an. Fehler, wenn ein Elternteil eine Datei ist.
async fn ensure_parents(tx: &mut Tx<'_>, user_id: i64, path: &str) -> ApiResult<()> {
    for dir in paths::ancestors(path) {
        match get_live(tx, user_id, dir).await? {
            Some(e) if e.is_dir => {}
            Some(_) => return Err(ApiError::Conflict(format!("'{dir}' ist eine Datei, kein Ordner"))),
            None => {
                let rev = next_rev(tx, user_id).await?;
                upsert(tx, user_id, dir, true, 0, None, None, now_ms(), rev).await?;
            }
        }
    }
    Ok(())
}

pub async fn get_live_pool(state: &AppState, user_id: i64, path: &str) -> ApiResult<Option<Entry>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND path = ? AND deleted = 0"
    ))
    .bind(user_id)
    .bind(path)
    .fetch_optional(&state.db)
    .await?)
}

// ---------------------------------------------------------------- list

#[derive(Deserialize)]
pub struct PathQuery {
    #[serde(default)]
    path: String,
}

#[derive(Serialize)]
pub struct ListResp {
    path: String,
    entries: Vec<ListEntry>,
}

#[derive(Serialize)]
pub struct ListEntry {
    name: String,
    #[serde(flatten)]
    entry: Entry,
}

/// Direkte Kinder eines Ordners (nicht gelöscht), Ordner zuerst.
pub async fn children(state: &AppState, user_id: i64, dir: &str) -> ApiResult<Vec<Entry>> {
    Ok(if dir.is_empty() {
        sqlx::query_as(&format!(
            "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND deleted = 0 AND instr(path, '/') = 0 \
             ORDER BY is_dir DESC, path COLLATE NOCASE"
        ))
        .bind(user_id)
        .fetch_all(&state.db)
        .await?
    } else {
        sqlx::query_as(&format!(
            "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND deleted = 0 \
             AND path LIKE ? ESCAPE '\\' AND instr(substr(path, ?), '/') = 0 \
             ORDER BY is_dir DESC, path COLLATE NOCASE"
        ))
        .bind(user_id)
        .bind(paths::like_prefix(dir))
        .bind(dir.len() as i64 + 2)
        .fetch_all(&state.db)
        .await?
    })
}

/// Alle Dateien unterhalb eines Ordners (für ZIP-Downloads).
pub async fn files_below(state: &AppState, user_id: i64, dir: &str) -> ApiResult<Vec<Entry>> {
    Ok(sqlx::query_as(&format!(
        "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND deleted = 0 AND is_dir = 0 \
         AND (? = '' OR path LIKE ? ESCAPE '\\') ORDER BY path"
    ))
    .bind(user_id)
    .bind(dir)
    .bind(paths::like_prefix(dir))
    .fetch_all(&state.db)
    .await?)
}

pub async fn list(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<PathQuery>,
) -> ApiResult<Json<ListResp>> {
    let dir = paths::normalize(&q.path)?;
    if !dir.is_empty() {
        match get_live_pool(&state, user.id, &dir).await? {
            Some(e) if e.is_dir => {}
            _ => return Err(ApiError::NotFound),
        }
    }
    let rows = children(&state, user.id, &dir).await?;
    let entries = rows
        .into_iter()
        .map(|e| ListEntry {
            name: e.path.rsplit('/').next().unwrap_or("").to_string(),
            entry: e,
        })
        .collect();
    Ok(Json(ListResp { path: dir, entries }))
}

// ---------------------------------------------------------------- upload

#[derive(Deserialize)]
pub struct UploadQuery {
    path: String,
    /// Änderungszeit der Datei auf dem Client (Unix-Millisekunden).
    mtime: Option<i64>,
    /// Optimistische Sperre: Revision, auf der die Änderung basiert.
    /// 0 = Datei darf noch nicht existieren. Weglassen = einfach überschreiben.
    base_rev: Option<i64>,
}

pub async fn upload(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<UploadQuery>,
    headers: HeaderMap,
    body: Body,
) -> ApiResult<(StatusCode, Json<Entry>)> {
    let path = paths::normalize_non_root(&q.path)?;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok());
    let (status, entry) = store_file(
        &state,
        user.id,
        path,
        body.into_data_stream(),
        declared,
        q.mtime.unwrap_or_else(now_ms),
        q.base_rev,
        &user.device_name,
    )
    .await?;
    Ok((status, Json(entry)))
}

/// Speichert eine hochgeladene Datei (App oder Gast-Upload über einen Link).
#[allow(clippy::too_many_arguments)]
pub async fn store_file<S, E>(
    state: &AppState,
    user_id: i64,
    path: String,
    stream: S,
    declared: Option<i64>,
    mtime: i64,
    base_rev: Option<i64>,
    by: &str,
) -> ApiResult<(StatusCode, Entry)>
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{

    // Grobe Vorprüfung, damit wir nicht unnötig hochladen.
    let (quota, used): (i64, i64) = sqlx::query_as("SELECT quota_bytes, used_bytes FROM users WHERE id = ?")
        .bind(user_id)
        .fetch_one(&state.db)
        .await?;
    let existing = get_live_pool(&state, user_id, &path).await?;
    if let Some(e) = &existing {
        if e.is_dir {
            return Err(ApiError::Conflict("Pfad ist ein Ordner".into()));
        }
    }
    check_base_rev(existing.as_ref(), base_rev)?;
    let old_size = existing.as_ref().map(|e| e.size).unwrap_or(0);
    let allowed = quota - used + old_size;
    // Größe vorab prüfen, wenn der Client sie mitschickt.
    if declared.map(|n| n > allowed).unwrap_or(false) {
        return Err(ApiError::QuotaExceeded);
    }

    // In eine temporäre Datei streamen, dabei hashen und Größe prüfen.
    let tmp_path = state.tmp_dir().join(uuid::Uuid::new_v4().to_string());
    let result = stream_to_file(stream, &tmp_path, allowed).await;
    let (size, sha) = match result {
        Ok(v) => v,
        Err(e) => {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Err(e);
        }
    };

    // Identischer Inhalt -> nichts speichern.
    if let Some(e) = &existing {
        if e.sha256.as_deref() == Some(sha.as_str()) && e.size == size {
            let _ = tokio::fs::remove_file(&tmp_path).await;
            return Ok((StatusCode::OK, e.clone()));
        }
    }

    let blob_id = uuid::Uuid::new_v4().to_string();
    let blob_path = state.user_blob_dir(user_id).join(&blob_id);
    tokio::fs::create_dir_all(state.user_blob_dir(user_id)).await?;
    tokio::fs::rename(&tmp_path, &blob_path).await?;

    let committed: ApiResult<(Entry, Option<String>)> = async {
        let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
        let current = get_live(&mut tx, user_id, &path).await?;
        if let Some(c) = &current {
            if c.is_dir {
                return Err(ApiError::Conflict("Pfad ist ein Ordner".into()));
            }
        }
        check_base_rev(current.as_ref(), base_rev)?;
        let old_size = current.as_ref().map(|c| c.size).unwrap_or(0);
        let delta = size - old_size;
        let r = sqlx::query(
            "UPDATE users SET used_bytes = used_bytes + ? WHERE id = ? AND used_bytes + ? <= quota_bytes",
        )
        .bind(delta)
        .bind(user_id)
        .bind(delta)
        .execute(&mut *tx)
        .await?;
        if r.rows_affected() == 0 && delta > 0 {
            return Err(ApiError::QuotaExceeded);
        }
        let since = cursor_now(&mut tx, user_id).await?;
        ensure_parents(&mut tx, user_id, &path).await?;
        let rev = next_rev(&mut tx, user_id).await?;
        upsert(&mut tx, user_id, &path, false, size, Some(&sha), Some(&blob_id), mtime, rev).await?;
        stamp(&mut tx, user_id, since, by).await?;
        let entry = get_live(&mut tx, user_id, &path).await?.ok_or(ApiError::NotFound)?;
        tx.commit().await?;
        Ok((entry, current.and_then(|c| c.blob_id)))
    }
    .await;

    match committed {
        Ok((entry, old_blob)) => {
            if let Some(old) = old_blob {
                let _ = tokio::fs::remove_file(state.user_blob_dir(user_id).join(old)).await;
            }
            let status = if existing.is_some() { StatusCode::OK } else { StatusCode::CREATED };
            Ok((status, entry))
        }
        Err(e) => {
            let _ = tokio::fs::remove_file(&blob_path).await;
            Err(e)
        }
    }
}

fn check_base_rev(current: Option<&Entry>, base_rev: Option<i64>) -> ApiResult<()> {
    match (base_rev, current) {
        (None, _) => Ok(()),
        (Some(0), None) => Ok(()),
        (Some(0), Some(_)) => Err(ApiError::Conflict("Datei existiert bereits".into())),
        (Some(b), Some(c)) if c.rev == b => Ok(()),
        (Some(_), _) => Err(ApiError::Conflict("Datei wurde inzwischen geändert".into())),
    }
}

async fn stream_to_file<S, E>(mut stream: S, path: &std::path::Path, max_bytes: i64) -> ApiResult<(i64, String)>
where
    S: futures_util::Stream<Item = Result<axum::body::Bytes, E>> + Unpin,
    E: std::fmt::Display,
{
    let file = tokio::fs::File::create(path).await?;
    let mut out = tokio::io::BufWriter::with_capacity(1 << 20, file);
    let mut hasher = Sha256::new();
    let mut size: i64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| ApiError::BadRequest(format!("Upload abgebrochen: {e}")))?;
        size += chunk.len() as i64;
        if size > max_bytes {
            return Err(ApiError::QuotaExceeded);
        }
        hasher.update(&chunk);
        out.write_all(&chunk).await?;
    }
    out.flush().await?;
    out.into_inner().sync_all().await?;
    Ok((size, hex::encode(hasher.finalize())))
}

// ---------------------------------------------------------------- download

pub async fn download(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<PathQuery>,
    headers: HeaderMap,
) -> ApiResult<Response> {
    let path = paths::normalize_non_root(&q.path)?;
    let entry = get_live_pool(&state, user.id, &path).await?.ok_or(ApiError::NotFound)?;
    if entry.is_dir {
        return Err(ApiError::BadRequest("Ordner können nicht heruntergeladen werden".into()));
    }
    serve_blob(&state, user.id, &entry, &headers, true).await
}

/// Liefert eine Datei aus, mit Unterstützung für `Range`-Anfragen.
pub async fn serve_blob(state: &AppState, user_id: i64, entry: &Entry, req: &HeaderMap, attachment: bool) -> ApiResult<Response> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let blob_id = entry.blob_id.clone().ok_or(ApiError::NotFound)?;
    let mut file = tokio::fs::File::open(state.user_blob_dir(user_id).join(blob_id)).await?;
    let total = entry.size as u64;
    let range = req.get(header::RANGE).and_then(|v| v.to_str().ok()).and_then(|r| parse_range(r, total));

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_TYPE, HeaderValue::from_static(guess_mime(&entry.path)));
    headers.insert(header::ACCEPT_RANGES, HeaderValue::from_static("bytes"));
    let name = entry.path.rsplit('/').next().unwrap_or("datei");
    let disp = if attachment { "attachment" } else { "inline" };
    if let Ok(v) = HeaderValue::from_str(&format!("{disp}; filename*=UTF-8''{}", pct_encode(name))) {
        headers.insert(header::CONTENT_DISPOSITION, v);
    }
    if let Some(sha) = &entry.sha256 {
        if let Ok(v) = HeaderValue::from_str(sha) {
            headers.insert("x-sha256", v);
        }
    }
    headers.insert("x-rev", HeaderValue::from(entry.rev));
    headers.insert("x-modified-at", HeaderValue::from(entry.modified_at));

    match range {
        Some((start, end)) => {
            file.seek(std::io::SeekFrom::Start(start)).await?;
            let len = end - start + 1;
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
            if let Ok(v) = HeaderValue::from_str(&format!("bytes {start}-{end}/{total}")) {
                headers.insert(header::CONTENT_RANGE, v);
            }
            let body = Body::from_stream(ReaderStream::with_capacity(file.take(len), 1 << 16));
            Ok((StatusCode::PARTIAL_CONTENT, headers, body).into_response())
        }
        None => {
            headers.insert(header::CONTENT_LENGTH, HeaderValue::from(total));
            Ok((headers, Body::from_stream(ReaderStream::with_capacity(file, 1 << 16))).into_response())
        }
    }
}

/// "bytes=a-b", "bytes=a-", "bytes=-n" -> (start, end) inklusive. Nur ein Bereich.
pub fn parse_range(h: &str, total: u64) -> Option<(u64, u64)> {
    let spec = h.strip_prefix("bytes=")?.trim();
    if spec.contains(',') || total == 0 {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (start, end) = if a.is_empty() {
        let n: u64 = b.parse().ok()?;
        (total.saturating_sub(n), total - 1)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { total - 1 } else { b.parse::<u64>().ok()?.min(total - 1) };
        (start, end)
    };
    (start <= end && start < total).then_some((start, end))
}

pub fn pct_encode(name: &str) -> String {
    name.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'.' | b'-' | b'_' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn guess_mime(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "pdf" => "application/pdf",
        "txt" | "md" | "csv" | "log" => "text/plain; charset=utf-8",
        "mp4" => "video/mp4",
        "webm" => "video/webm",
        "mov" => "video/quicktime",
        "mp3" => "audio/mpeg",
        "wav" => "audio/wav",
        "ogg" => "audio/ogg",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

// ---------------------------------------------------------------- mkdir / delete / move

#[derive(Deserialize)]
pub struct PathBody {
    path: String,
}

pub async fn mkdir(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<PathBody>,
) -> ApiResult<(StatusCode, Json<Entry>)> {
    let path = paths::normalize_non_root(&req.path)?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    if let Some(e) = get_live(&mut tx, user.id, &path).await? {
        return if e.is_dir {
            Ok((StatusCode::OK, Json(e)))
        } else {
            Err(ApiError::Conflict("Es gibt schon eine Datei mit diesem Namen".into()))
        };
    }
    let since = cursor_now(&mut tx, user.id).await?;
    ensure_parents(&mut tx, user.id, &path).await?;
    let rev = next_rev(&mut tx, user.id).await?;
    upsert(&mut tx, user.id, &path, true, 0, None, None, now_ms(), rev).await?;
    stamp(&mut tx, user.id, since, &user.device_name).await?;
    let e = get_live(&mut tx, user.id, &path).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(e)))
}

pub async fn delete(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<PathBody>,
) -> ApiResult<Json<serde_json::Value>> {
    let path = paths::normalize_non_root(&req.path)?;
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let subtree = get_subtree(&mut tx, user.id, &path).await?;
    if subtree.is_empty() {
        return Err(ApiError::NotFound);
    }
    let since = cursor_now(&mut tx, user.id).await?;
    let mut freed = 0i64;
    let mut blobs = Vec::new();
    // Kinder zuerst als gelöscht markieren.
    for e in subtree.iter().rev() {
        let rev = next_rev(&mut tx, user.id).await?;
        tombstone(&mut tx, user.id, &e.path, rev).await?;
        freed += e.size;
        if let Some(b) = &e.blob_id {
            blobs.push(b.clone());
        }
    }
    sqlx::query("UPDATE users SET used_bytes = MAX(0, used_bytes - ?) WHERE id = ?")
        .bind(freed)
        .bind(user.id)
        .execute(&mut *tx)
        .await?;
    stamp(&mut tx, user.id, since, &user.device_name).await?;
    tx.commit().await?;
    for b in blobs {
        let _ = tokio::fs::remove_file(state.user_blob_dir(user.id).join(b)).await;
    }
    Ok(Json(serde_json::json!({ "deleted": subtree.len(), "freed_bytes": freed })))
}

#[derive(Deserialize)]
pub struct MoveBody {
    from: String,
    to: String,
}

pub async fn move_entry(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<MoveBody>,
) -> ApiResult<Json<Entry>> {
    let from = paths::normalize_non_root(&req.from)?;
    let to = paths::normalize_non_root(&req.to)?;
    if from == to {
        return Err(ApiError::BadRequest("Quelle und Ziel sind gleich".into()));
    }
    if to.starts_with(&format!("{from}/")) {
        return Err(ApiError::BadRequest("Ordner kann nicht in sich selbst verschoben werden".into()));
    }
    let mut tx = state.db.begin_with("BEGIN IMMEDIATE").await?;
    let subtree = get_subtree(&mut tx, user.id, &from).await?;
    if subtree.is_empty() {
        return Err(ApiError::NotFound);
    }
    if get_live(&mut tx, user.id, &to).await?.is_some() {
        return Err(ApiError::Conflict("Ziel existiert bereits".into()));
    }
    let since = cursor_now(&mut tx, user.id).await?;
    ensure_parents(&mut tx, user.id, &to).await?;
    for e in &subtree {
        let new_path = format!("{to}{}", &e.path[from.len()..]);
        let rev = next_rev(&mut tx, user.id).await?;
        upsert(
            &mut tx,
            user.id,
            &new_path,
            e.is_dir,
            e.size,
            e.sha256.as_deref(),
            e.blob_id.as_deref(),
            e.modified_at,
            rev,
        )
        .await?;
    }
    for e in subtree.iter().rev() {
        let rev = next_rev(&mut tx, user.id).await?;
        tombstone(&mut tx, user.id, &e.path, rev).await?;
    }
    stamp(&mut tx, user.id, since, &user.device_name).await?;
    let moved = get_live(&mut tx, user.id, &to).await?.ok_or(ApiError::NotFound)?;
    tx.commit().await?;
    Ok(Json(moved))
}

// ---------------------------------------------------------------- sync feed

#[derive(Deserialize)]
pub struct ChangesQuery {
    #[serde(default)]
    cursor: i64,
    limit: Option<i64>,
}

#[derive(Serialize)]
pub struct ChangesResp {
    entries: Vec<Entry>,
    cursor: i64,
    has_more: bool,
}

/// Alle Änderungen seit `cursor`. Mit cursor=0 bekommt der Client den
/// kompletten aktuellen Stand (ohne Grabsteine).
pub async fn changes(
    State(state): State<AppState>,
    user: AuthUser,
    Query(q): Query<ChangesQuery>,
) -> ApiResult<Json<ChangesResp>> {
    let limit = q.limit.unwrap_or(1000).clamp(1, 5000);
    let include_deleted = q.cursor > 0;
    // Erst den aktuellen Stand lesen, dann nur bis dahin abfragen. So geht
    // keine Änderung verloren, die zwischen den beiden Abfragen committed wird.
    let (head,): (i64,) = sqlx::query_as("SELECT cursor FROM users WHERE id = ?")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    let mut entries: Vec<Entry> = sqlx::query_as(&format!(
        "SELECT {ENTRY_COLS} FROM files WHERE user_id = ? AND rev > ? AND rev <= ? AND (deleted = 0 OR ?) \
         ORDER BY rev LIMIT ?"
    ))
    .bind(user.id)
    .bind(q.cursor)
    .bind(head)
    .bind(include_deleted)
    .bind(limit + 1)
    .fetch_all(&state.db)
    .await?;
    let has_more = entries.len() as i64 > limit;
    entries.truncate(limit as usize);
    let cursor = match entries.last() {
        Some(e) if has_more => e.rev,
        _ => head.max(q.cursor),
    };
    Ok(Json(ChangesResp { entries, cursor, has_more }))
}
