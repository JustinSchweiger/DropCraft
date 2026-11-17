use axum::{
    extract::{Path, State},
    http::StatusCode,
    Json,
};
use serde::{Deserialize, Serialize};

use crate::{
    auth::{self, AuthUser},
    error::{ApiError, ApiResult},
    AppState,
};

const GB: i64 = 1024 * 1024 * 1024;

fn require_admin(user: &AuthUser) -> ApiResult<()> {
    if user.is_admin {
        Ok(())
    } else {
        Err(ApiError::Forbidden)
    }
}

#[derive(Serialize, sqlx::FromRow)]
pub struct UserInfo {
    id: i64,
    username: String,
    is_admin: bool,
    quota_bytes: i64,
    used_bytes: i64,
    created_at: i64,
}

pub async fn list_users(State(state): State<AppState>, user: AuthUser) -> ApiResult<Json<Vec<UserInfo>>> {
    require_admin(&user)?;
    let users = sqlx::query_as(
        "SELECT id, username, is_admin, quota_bytes, used_bytes, created_at FROM users ORDER BY id",
    )
    .fetch_all(&state.db)
    .await?;
    Ok(Json(users))
}

#[derive(Deserialize)]
pub struct CreateUserReq {
    username: String,
    password: String,
    /// Speicherlimit in GB. Standard: DROPCRAFT_DEFAULT_QUOTA_GB.
    quota_gb: Option<f64>,
    #[serde(default)]
    is_admin: bool,
}

pub async fn create_user(
    State(state): State<AppState>,
    user: AuthUser,
    Json(req): Json<CreateUserReq>,
) -> ApiResult<(StatusCode, Json<serde_json::Value>)> {
    require_admin(&user)?;
    let quota = req
        .quota_gb
        .map(|g| (g * GB as f64) as i64)
        .unwrap_or(crate::settings::load(&state).await.default_quota_bytes());
    if quota < 0 {
        return Err(ApiError::BadRequest("Quota darf nicht negativ sein".into()));
    }
    let id = auth::create_user(&state, &req.username, &req.password, quota, req.is_admin).await?;
    Ok((StatusCode::CREATED, Json(serde_json::json!({ "id": id }))))
}

#[derive(Deserialize)]
pub struct UpdateUserReq {
    quota_gb: Option<f64>,
    password: Option<String>,
}

pub async fn update_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
    Json(req): Json<UpdateUserReq>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if let Some(g) = req.quota_gb {
        let r = sqlx::query("UPDATE users SET quota_bytes = ? WHERE id = ?")
            .bind((g * GB as f64) as i64)
            .bind(id)
            .execute(&state.db)
            .await?;
        if r.rows_affected() == 0 {
            return Err(ApiError::NotFound);
        }
    }
    if let Some(pw) = req.password {
        auth::validate_password(&pw, crate::settings::load(&state).await.min_password_len)?;
        auth::set_password(&state, id, &pw, None).await?;
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

pub async fn delete_user(
    State(state): State<AppState>,
    user: AuthUser,
    Path(id): Path<i64>,
) -> ApiResult<Json<serde_json::Value>> {
    require_admin(&user)?;
    if id == user.id {
        return Err(ApiError::BadRequest("Du kannst dich nicht selbst löschen".into()));
    }
    if !remove_user(&state, id).await? {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "ok": true })))
}

/// Löscht ein Konto samt Dateien. `false`, wenn es das Konto nicht gab.
pub async fn remove_user(state: &AppState, id: i64) -> ApiResult<bool> {
    let r = sqlx::query("DELETE FROM users WHERE id = ?").bind(id).execute(&state.db).await?;
    if r.rows_affected() == 0 {
        return Ok(false);
    }
    let _ = tokio::fs::remove_dir_all(state.user_dir(id)).await;
    Ok(true)
}
