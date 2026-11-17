use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{
    extract::{FromRequestParts, State},
    http::request::Parts,
    Json,
};
use rand::RngCore;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    db::now_ms,
    error::{ApiError, ApiResult},
    AppState,
};

pub fn hash_password(pw: &str) -> anyhow::Result<String> {
    let salt = SaltString::generate(&mut OsRng);
    Ok(Argon2::default()
        .hash_password(pw.as_bytes(), &salt)
        .map_err(|e| anyhow::anyhow!("hash: {e}"))?
        .to_string())
}

pub fn verify_password(pw: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .map(|h| Argon2::default().verify_password(pw.as_bytes(), &h).is_ok())
        .unwrap_or(false)
}

pub(crate) fn hash_token(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

pub fn validate_username(name: &str) -> ApiResult<()> {
    let ok = (3..=32).contains(&name.len())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.');
    if ok {
        Ok(())
    } else {
        Err(ApiError::BadRequest(
            "Benutzername: 3–32 Zeichen, nur Buchstaben, Zahlen, . _ -".into(),
        ))
    }
}

pub fn validate_password(pw: &str, min: u32) -> ApiResult<()> {
    if pw.chars().count() < min as usize {
        return Err(ApiError::BadRequest(format!("Password needs at least {min} characters / Passwort braucht mindestens {min} Zeichen")));
    }
    Ok(())
}

/// Prüft Benutzername und Passwort (gleich lange Antwortzeit auch für
/// unbekannte Namen). Gesperrte Konten: `Forbidden`.
pub async fn check_login(state: &AppState, username: &str, password: &str) -> ApiResult<i64> {
    let row: Option<(i64, String, bool)> = sqlx::query_as("SELECT id, password_hash, disabled FROM users WHERE username = ?")
        .bind(username)
        .fetch_optional(&state.db)
        .await?;
    let Some((id, hash, disabled)) = row else {
        let pw = password.to_string();
        let dummy = state.dummy_hash.clone();
        let _ = tokio::task::spawn_blocking(move || verify_password(&pw, &dummy)).await;
        return Err(ApiError::Unauthorized);
    };
    let pw = password.to_string();
    let ok = tokio::task::spawn_blocking(move || verify_password(&pw, &hash))
        .await
        .map_err(|e| ApiError::Internal(e.into()))?;
    if !ok {
        return Err(ApiError::Unauthorized);
    }
    if disabled {
        return Err(ApiError::Forbidden);
    }
    Ok(id)
}

/// Neue Sitzung (App, Browser oder Konto-Website); gibt das Token zurück.
pub async fn create_session(state: &AppState, user_id: i64, device_name: &str) -> ApiResult<String> {
    let mut raw = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut raw);
    let token = hex::encode(raw);
    let now = now_ms();
    sqlx::query("INSERT INTO sessions (token_hash, user_id, device_name, created_at, last_used) VALUES (?, ?, ?, ?, ?)")
        .bind(hash_token(&token))
        .bind(user_id)
        .bind(device_name)
        .bind(now)
        .bind(now)
        .execute(&state.db)
        .await?;
    Ok(token)
}

/// Setzt ein neues Passwort und meldet alle Sitzungen ab (außer `keep`).
pub async fn set_password(state: &AppState, user_id: i64, password: &str, keep: Option<&str>) -> ApiResult<()> {
    sqlx::query("UPDATE users SET password_hash = ? WHERE id = ?")
        .bind(hash_password(password)?)
        .bind(user_id)
        .execute(&state.db)
        .await?;
    sqlx::query("DELETE FROM sessions WHERE user_id = ? AND token_hash != ?")
        .bind(user_id)
        .bind(keep.unwrap_or(""))
        .execute(&state.db)
        .await?;
    Ok(())
}

/// Angemeldeter Nutzer, aus dem `Authorization: Bearer <token>` Header.
#[derive(Clone, Debug)]
pub struct AuthUser {
    pub id: i64,
    pub is_admin: bool,
    pub token_hash: String,
    /// Name des angemeldeten Geräts (für „geändert von …“).
    pub device_name: String,
}

impl FromRequestParts<AppState> for AuthUser {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &AppState) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .ok_or(ApiError::Unauthorized)?;
        let token = header.strip_prefix("Bearer ").ok_or(ApiError::Unauthorized)?;
        let token_hash = hash_token(token.trim());
        let row: Option<(i64, i64, i64, String)> = sqlx::query_as(
            "SELECT u.id, u.is_admin, s.last_used, s.device_name FROM sessions s JOIN users u ON u.id = s.user_id WHERE s.token_hash = ? AND u.disabled = 0",
        )
        .bind(&token_hash)
        .fetch_optional(&state.db)
        .await?;
        let (id, is_admin, last_used, device_name) = row.ok_or(ApiError::Unauthorized)?;
        // "Zuletzt benutzt" nur grob (stündlich) aktualisieren, um Schreibzugriffe zu sparen.
        let now = now_ms();
        if now - last_used > 3_600_000 {
            let _ = sqlx::query("UPDATE sessions SET last_used = ? WHERE token_hash = ?")
                .bind(now)
                .bind(&token_hash)
                .execute(&state.db)
                .await;
        }
        Ok(AuthUser { id, is_admin: is_admin != 0, token_hash, device_name })
    }
}

pub async fn create_user(
    state: &AppState,
    username: &str,
    password: &str,
    quota_bytes: i64,
    is_admin: bool,
) -> ApiResult<i64> {
    validate_username(username)?;
    validate_password(password, crate::settings::load(state).await.min_password_len)?;
    let hash = hash_password(password)?;
    let res = sqlx::query(
        "INSERT INTO users (username, password_hash, is_admin, quota_bytes, created_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(username)
    .bind(hash)
    .bind(is_admin as i64)
    .bind(quota_bytes)
    .bind(now_ms())
    .execute(&state.db)
    .await;
    match res {
        Ok(r) => {
            let id = r.last_insert_rowid();
            tokio::fs::create_dir_all(state.user_blob_dir(id)).await?;
            Ok(id)
        }
        Err(sqlx::Error::Database(e)) if e.is_unique_violation() => {
            Err(ApiError::Conflict("Benutzername ist schon vergeben".into()))
        }
        Err(e) => Err(e.into()),
    }
}

#[derive(Deserialize)]
pub struct RegisterReq {
    username: String,
    password: String,
}

/// Freie Registrierung, nur wenn DROPCRAFT_OPEN_REGISTRATION=true.
/// Den ersten Admin legt man auf dem Server per `dropcraft-server adduser`
/// an, damit niemand aus dem Internet ihn sich schnappen kann.
pub async fn register(State(state): State<AppState>, Json(req): Json<RegisterReq>) -> ApiResult<Json<serde_json::Value>> {
    let cfg = crate::settings::load(&state).await;
    if !cfg.open_registration {
        return Err(ApiError::Forbidden);
    }
    let id = create_user(&state, &req.username, &req.password, cfg.default_quota_bytes(), false).await?;
    Ok(Json(serde_json::json!({ "id": id, "is_admin": false })))
}

#[derive(Deserialize)]
pub struct LoginReq {
    username: String,
    password: String,
    #[serde(default)]
    device_name: Option<String>,
}

#[derive(Serialize)]
pub struct LoginResp {
    token: String,
}

pub async fn login(State(state): State<AppState>, Json(req): Json<LoginReq>) -> ApiResult<Json<LoginResp>> {
    let id = check_login(&state, &req.username, &req.password).await?;
    let token = create_session(&state, id, req.device_name.as_deref().unwrap_or("unbekannt")).await?;
    Ok(Json(LoginResp { token }))
}

pub async fn logout(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<serde_json::Value>> {
    sqlx::query("DELETE FROM sessions WHERE token_hash = ?")
        .bind(&user.token_hash)
        .execute(&state.db)
        .await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}

#[derive(Serialize, sqlx::FromRow)]
pub struct Me {
    id: i64,
    username: String,
    is_admin: bool,
    quota_bytes: i64,
    used_bytes: i64,
    cursor: i64,
}

pub async fn me(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Me>> {
    let me: Me = sqlx::query_as(
        "SELECT id, username, is_admin, quota_bytes, used_bytes, cursor FROM users WHERE id = ?",
    )
    .bind(user.id)
    .fetch_one(&state.db)
    .await?;
    Ok(Json(me))
}

#[derive(Deserialize)]
pub struct ChangePasswordReq {
    old_password: String,
    new_password: String,
}

pub async fn change_password(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<ChangePasswordReq>,
) -> ApiResult<Json<serde_json::Value>> {
    validate_password(&req.new_password, crate::settings::load(&state).await.min_password_len)?;
    let (hash,): (String,) = sqlx::query_as("SELECT password_hash FROM users WHERE id = ?")
        .bind(user.id)
        .fetch_one(&state.db)
        .await?;
    if !verify_password(&req.old_password, &hash) {
        return Err(ApiError::Unauthorized);
    }
    // Alle anderen Geräte abmelden.
    set_password(&state, user.id, &req.new_password, Some(&user.token_hash)).await?;
    Ok(Json(serde_json::json!({ "ok": true })))
}
