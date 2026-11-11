mod admin;
mod auth;
mod db;
mod error;
mod files;
mod paths;
mod shares;

use std::{net::SocketAddr, path::PathBuf, sync::Arc};

use axum::{
    extract::DefaultBodyLimit,
    routing::{delete, get, patch, post, put},
    Json, Router,
};
use sqlx::SqlitePool;
use tower_http::trace::TraceLayer;
use tracing_subscriber::EnvFilter;

#[derive(Clone, Debug)]
pub struct Config {
    pub data_dir: PathBuf,
    pub bind: SocketAddr,
    pub open_registration: bool,
    pub default_quota_bytes: i64,
}

impl Config {
    fn from_env() -> anyhow::Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
        let quota_gb: f64 = get("DROPCRAFT_DEFAULT_QUOTA_GB").map(|v| v.parse()).transpose()?.unwrap_or(20.0);
        Ok(Config {
            data_dir: get("DROPCRAFT_DATA").unwrap_or_else(|| "./data".into()).into(),
            bind: get("DROPCRAFT_BIND").unwrap_or_else(|| "0.0.0.0:8080".into()).parse()?,
            open_registration: get("DROPCRAFT_OPEN_REGISTRATION")
                .map(|v| v == "true" || v == "1")
                .unwrap_or(false),
            default_quota_bytes: (quota_gb * 1024.0 * 1024.0 * 1024.0) as i64,
        })
    }
}

#[derive(Clone)]
pub struct AppState {
    pub db: SqlitePool,
    pub cfg: Arc<Config>,
    pub dummy_hash: Arc<String>,
}

impl AppState {
    pub fn user_dir(&self, user_id: i64) -> PathBuf {
        self.cfg.data_dir.join("users").join(user_id.to_string())
    }
    pub fn user_blob_dir(&self, user_id: i64) -> PathBuf {
        self.user_dir(user_id).join("blobs")
    }
    pub fn tmp_dir(&self) -> PathBuf {
        self.cfg.data_dir.join("tmp")
    }
}

pub fn router(state: AppState) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { Json(serde_json::json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") })) }))
        .route("/auth/register", post(auth::register))
        .route("/auth/login", post(auth::login))
        .route("/auth/logout", post(auth::logout))
        .route("/auth/password", post(auth::change_password))
        .route("/me", get(auth::me))
        .route("/files/list", get(files::list))
        .route("/files/download", get(files::download))
        .route("/files/upload", put(files::upload).layer(DefaultBodyLimit::disable()))
        .route("/files/mkdir", post(files::mkdir))
        .route("/files/delete", post(files::delete))
        .route("/files/move", post(files::move_entry))
        .route("/sync/changes", get(files::changes))
        .route("/admin/users", get(admin::list_users).post(admin::create_user))
        .route("/admin/users/{id}", patch(admin::update_user).delete(admin::delete_user))
        .route("/shares", get(shares::list).post(shares::create))
        .route("/shares/{token}", delete(shares::delete));
    let public = Router::new()
        .route("/s/{token}", get(shares::page))
        .route("/s/{token}/file", get(shares::file))
        .route("/s/{token}/zip", get(shares::zip));
    Router::new()
        .nest("/api", api)
        .merge(public)
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn build_state(cfg: Config, clean_tmp: bool) -> anyhow::Result<AppState> {
    tokio::fs::create_dir_all(cfg.data_dir.join("users")).await?;
    let tmp = cfg.data_dir.join("tmp");
    if clean_tmp {
        // Reste abgebrochener Uploads wegräumen (nur beim Serverstart).
        let _ = tokio::fs::remove_dir_all(&tmp).await;
    }
    tokio::fs::create_dir_all(&tmp).await?;
    let db = db::connect(&cfg.data_dir).await?;
    let dummy_hash = auth::hash_password("dummy-password-for-timing")?;
    Ok(AppState { db, cfg: Arc::new(cfg), dummy_hash: Arc::new(dummy_hash) })
}

/// `dropcraft-server adduser <name> <quota_gb> [--admin]`
/// Passwort wird über stdin gelesen (oder DROPCRAFT_PASSWORD).
async fn cli_adduser(state: &AppState, args: &[String]) -> anyhow::Result<()> {
    let name = args.first().ok_or_else(|| anyhow::anyhow!("Benutzername fehlt"))?;
    let quota_gb: f64 = args.get(1).map(|s| s.parse()).transpose()?.unwrap_or(20.0);
    let admin = args.iter().any(|a| a == "--admin");
    let pw = match std::env::var("DROPCRAFT_PASSWORD") {
        Ok(p) => p,
        Err(_) => {
            eprint!("Passwort für {name}: ");
            let mut s = String::new();
            std::io::stdin().read_line(&mut s)?;
            s.trim_end_matches(['\r', '\n']).to_string()
        }
    };
    let quota = (quota_gb * 1024.0 * 1024.0 * 1024.0) as i64;
    match auth::create_user(state, name, &pw, quota, admin).await {
        Ok(id) => println!("Account '{name}' angelegt (id {id}, {quota_gb} GB{}).", if admin { ", Admin" } else { "" }),
        Err(e) => anyhow::bail!("Fehler: {e:?}"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "dropcraft_server=info,tower_http=info".into()))
        .init();

    let cfg = Config::from_env()?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("adduser") {
        let state = build_state(cfg, false).await?;
        return cli_adduser(&state, &args[1..]).await;
    }
    let state = build_state(cfg.clone(), true).await?;

    let listener = tokio::net::TcpListener::bind(cfg.bind).await?;
    tracing::info!("DropCraft-Server läuft auf {} (Daten: {})", cfg.bind, cfg.data_dir.display());
    axum::serve(listener, router(state))
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { _ = ctrl_c => {}, _ = term => {} }
    tracing::info!("Fahre herunter…");
}
