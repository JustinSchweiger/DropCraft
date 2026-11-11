//! Lokale Datenbank: Was war beim letzten erfolgreichen Abgleich auf
//! beiden Seiten gleich? Daraus erkennt die Sync-Engine, auf welcher Seite
//! sich etwas geändert hat.

use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, PartialEq)]
pub struct Synced {
    pub path: String,
    pub is_dir: bool,
    pub rev: i64,
    pub sha256: Option<String>,
    pub size: i64,
    /// Änderungszeit der lokalen Datei nach dem Abgleich (ms).
    pub local_mtime: i64,
}

pub struct State {
    db: Connection,
}

impl State {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let db = Connection::open(path)?;
        db.execute_batch(
            "PRAGMA journal_mode = WAL;
             CREATE TABLE IF NOT EXISTS files (
                path TEXT PRIMARY KEY, is_dir INTEGER NOT NULL, rev INTEGER NOT NULL,
                sha256 TEXT, size INTEGER NOT NULL, local_mtime INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        Ok(Self { db })
    }

    pub fn cursor(&self) -> i64 {
        self.meta("cursor").and_then(|v| v.parse().ok()).unwrap_or(0)
    }

    pub fn set_cursor(&self, c: i64) -> rusqlite::Result<()> {
        self.set_meta("cursor", &c.to_string())
    }

    pub fn meta(&self, key: &str) -> Option<String> {
        self.db
            .query_row("SELECT value FROM meta WHERE key = ?", [key], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
    }

    pub fn set_meta(&self, key: &str, value: &str) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = ?2",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn get(&self, path: &str) -> Option<Synced> {
        self.db
            .query_row(
                "SELECT path, is_dir, rev, sha256, size, local_mtime FROM files WHERE path = ?",
                [path],
                Self::row,
            )
            .optional()
            .ok()
            .flatten()
    }

    pub fn all(&self) -> rusqlite::Result<Vec<Synced>> {
        let mut st = self
            .db
            .prepare("SELECT path, is_dir, rev, sha256, size, local_mtime FROM files ORDER BY path")?;
        let rows = st.query_map([], Self::row)?;
        rows.collect()
    }

    pub fn put(&self, s: &Synced) -> rusqlite::Result<()> {
        self.db.execute(
            "INSERT INTO files (path, is_dir, rev, sha256, size, local_mtime) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(path) DO UPDATE SET is_dir = ?2, rev = ?3, sha256 = ?4, size = ?5, local_mtime = ?6",
            params![s.path, s.is_dir, s.rev, s.sha256, s.size, s.local_mtime],
        )?;
        Ok(())
    }

    /// Entfernt einen Eintrag und alle darunterliegenden.
    pub fn remove_tree(&self, path: &str) -> rusqlite::Result<()> {
        let like = format!("{}/%", path.replace('\\', "\\\\").replace('%', "\\%").replace('_', "\\_"));
        self.db.execute(
            "DELETE FROM files WHERE path = ?1 OR path LIKE ?2 ESCAPE '\\'",
            params![path, like],
        )?;
        Ok(())
    }

    pub fn clear(&self) -> rusqlite::Result<()> {
        self.db.execute_batch("DELETE FROM files; DELETE FROM meta;")
    }

    fn row(r: &rusqlite::Row) -> rusqlite::Result<Synced> {
        Ok(Synced {
            path: r.get(0)?,
            is_dir: r.get(1)?,
            rev: r.get(2)?,
            sha256: r.get(3)?,
            size: r.get(4)?,
            local_mtime: r.get(5)?,
        })
    }
}
