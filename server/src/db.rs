use std::{path::Path, str::FromStr, time::Duration};

use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous},
    SqlitePool,
};

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS users (
    id            INTEGER PRIMARY KEY AUTOINCREMENT,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    is_admin      INTEGER NOT NULL DEFAULT 0,
    quota_bytes   INTEGER NOT NULL,
    used_bytes    INTEGER NOT NULL DEFAULT 0,
    cursor        INTEGER NOT NULL DEFAULT 0,
    created_at    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS sessions (
    token_hash  TEXT PRIMARY KEY,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_name TEXT NOT NULL,
    created_at  INTEGER NOT NULL,
    last_used   INTEGER NOT NULL
);

-- Ein Eintrag pro Datei/Ordner und Account. Gelöschte Einträge bleiben als
-- "Grabstein" (deleted = 1) erhalten, damit Clients die Löschung per
-- Änderungs-Feed mitbekommen. `rev` ist ein pro Account steigender Zähler.
CREATE TABLE IF NOT EXISTS files (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    user_id     INTEGER NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    path        TEXT NOT NULL,
    is_dir      INTEGER NOT NULL,
    size        INTEGER NOT NULL DEFAULT 0,
    sha256      TEXT,
    blob_id     TEXT,
    modified_at INTEGER NOT NULL,
    rev         INTEGER NOT NULL,
    deleted     INTEGER NOT NULL DEFAULT 0,
    UNIQUE (user_id, path)
);

CREATE INDEX IF NOT EXISTS files_user_rev ON files (user_id, rev);
"#;

pub async fn connect(data_dir: &Path) -> anyhow::Result<SqlitePool> {
    let db_path = data_dir.join("dropcraft.db");
    let opts = SqliteConnectOptions::from_str(&format!("sqlite://{}", db_path.display()))?
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(8)
        .connect_with(opts)
        .await?;
    sqlx::raw_sql(SCHEMA).execute(&pool).await?;
    sqlx::raw_sql(crate::shares::SCHEMA).execute(&pool).await?;
    // Spätere Erweiterungen bestehender Datenbanken
    add_column(&pool, "files", "modified_by", "TEXT").await?;
    add_column(&pool, "shares", "allow_upload", "INTEGER NOT NULL DEFAULT 0").await?;
    add_column(&pool, "shares", "password_hash", "TEXT").await?;
    add_column(&pool, "shares", "expires_at", "INTEGER").await?;
    add_column(&pool, "shares", "notify", "INTEGER NOT NULL DEFAULT 1").await?;
    add_column(&pool, "users", "disabled", "INTEGER NOT NULL DEFAULT 0").await?;
    sqlx::raw_sql("CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL)")
        .execute(&pool)
        .await?;
    Ok(pool)
}

async fn add_column(pool: &SqlitePool, table: &str, col: &str, decl: &str) -> anyhow::Result<()> {
    let cols: Vec<(String,)> = sqlx::query_as(&format!("SELECT name FROM pragma_table_info('{table}')")).fetch_all(pool).await?;
    if !cols.iter().any(|(n,)| n == col) {
        sqlx::raw_sql(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl}")).execute(pool).await?;
    }
    Ok(())
}

/// Zufälliges Server-Geheimnis (für Cookies von passwortgeschützten Links).
pub async fn secret(pool: &SqlitePool) -> anyhow::Result<String> {
    if let Some((v,)) = sqlx::query_as::<_, (String,)>("SELECT value FROM meta WHERE key = 'secret'").fetch_optional(pool).await? {
        return Ok(v);
    }
    let mut raw = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut raw);
    let v = hex::encode(raw);
    sqlx::query("INSERT OR IGNORE INTO meta (key, value) VALUES ('secret', ?)").bind(&v).execute(pool).await?;
    Ok(v)
}

pub fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
