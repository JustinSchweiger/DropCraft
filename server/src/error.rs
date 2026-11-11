use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde_json::json;

#[derive(Debug)]
pub enum ApiError {
    BadRequest(String),
    Unauthorized,
    Forbidden,
    NotFound,
    Conflict(String),
    QuotaExceeded,
    Internal(anyhow::Error),
}

pub type ApiResult<T> = Result<T, ApiError>;

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, code, msg) = match self {
            ApiError::BadRequest(m) => (StatusCode::BAD_REQUEST, "bad_request", m),
            ApiError::Unauthorized => (StatusCode::UNAUTHORIZED, "unauthorized", "Nicht angemeldet".into()),
            ApiError::Forbidden => (StatusCode::FORBIDDEN, "forbidden", "Keine Berechtigung".into()),
            ApiError::NotFound => (StatusCode::NOT_FOUND, "not_found", "Nicht gefunden".into()),
            ApiError::Conflict(m) => (StatusCode::CONFLICT, "conflict", m),
            ApiError::QuotaExceeded => (
                StatusCode::INSUFFICIENT_STORAGE,
                "quota_exceeded",
                "Speicherplatz voll".into(),
            ),
            ApiError::Internal(e) => {
                tracing::error!("internal error: {e:#}");
                (StatusCode::INTERNAL_SERVER_ERROR, "internal", "Interner Fehler".into())
            }
        };
        (status, Json(json!({ "error": code, "message": msg }))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(e: sqlx::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl From<std::io::Error> for ApiError {
    fn from(e: std::io::Error) -> Self {
        ApiError::Internal(e.into())
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        ApiError::Internal(e)
    }
}
