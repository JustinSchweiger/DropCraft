//! Befehle aus dem Explorer-Menü, die ohne Fenster auskommen. Sie laufen in
//! einem eigenen, kurzlebigen Prozess (die laufende App bleibt unberührt).

use std::path::Path;

use dropcraft_core::{Api, ApiError};

use crate::{
    app::{copy_to_clipboard, open_url, remote_path},
    i18n::t,
    config::{Account, Config},
    notify,
};

/// Konto und Server-Pfad zu einem lokalen Pfad. `Some("")` = der Sync-Ordner selbst.
fn locate(path: &Path) -> Option<(Account, String)> {
    let cfg = Config::load();
    cfg.accounts.into_iter().find_map(|acc| {
        if same_dir(&acc.sync_dir, path) {
            return Some((acc, String::new()));
        }
        let rp = remote_path(&acc.sync_dir, path)?;
        Some((acc, rp))
    })
}

fn same_dir(a: &Path, b: &Path) -> bool {
    let n = |p: &Path| p.to_string_lossy().replace('/', "\\").trim_end_matches('\\').to_lowercase();
    n(a) == n(b)
}

fn name(path: &Path) -> String {
    path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| path.display().to_string())
}

/// „DropCraft-Link kopieren“: Link anlegen (oder vorhandenen nehmen), in die
/// Zwischenablage legen und mit einer Windows-Meldung bestätigen.
pub fn copy_link(path: &Path) {
    let Some((acc, rp)) = locate(path).filter(|(_, rp)| !rp.is_empty()) else {
        notify::show("DropCraft", &tf!("“{}” is not in a DropCraft folder.", "„{}“ liegt in keinem DropCraft-Ordner.", name(path)));
        return;
    };
    let res = Api::new(&acc.server, acc.token.clone()).and_then(|api| api.create_share(&rp).map(|s| api.share_url(&s.token)));
    match res {
        Ok(url) if copy_to_clipboard(&url) => notify::show(t("Link copied", "Link kopiert"), &tf!("“{}” · {url}", "„{}“ · {url}", name(path))),
        Ok(url) => notify::show(t("Link created", "Link erstellt"), &tf!("Clipboard not available: {url}", "Zwischenablage nicht verfügbar: {url}")),
        Err(ApiError::NotFound) => notify::show("DropCraft", &tf!("“{}” is not uploaded yet – try again in a moment.", "„{}“ ist noch nicht hochgeladen – gleich noch einmal versuchen.", name(path))),
        Err(ApiError::Unauthorized) => notify::show("DropCraft", t("Please sign in to DropCraft again.", "Bitte in DropCraft neu anmelden.")),
        Err(e) => notify::show(t("Could not create the link", "Link konnte nicht erstellt werden"), &e.to_string()),
    }
}

/// „Im Browser anzeigen“: öffnet die Web-App im Ordner (bei Dateien: im
/// Ordner, in dem sie liegt).
pub fn open_web(path: &Path) {
    let Some((acc, rp)) = locate(path) else {
        notify::show("DropCraft", &tf!("“{}” is not in a DropCraft folder.", "„{}“ liegt in keinem DropCraft-Ordner.", name(path)));
        return;
    };
    let dir = if path.is_dir() { rp } else { rp.rsplit_once('/').map(|(d, _)| d.to_string()).unwrap_or_default() };
    let server = acc.server.trim_end_matches('/');
    open_url(&if dir.is_empty() { format!("{server}/app/#/files") } else { format!("{server}/app/#/files/{}", encode(&dir)) });
}

/// Wie `encodeURIComponent` im Browser.
fn encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.!~*'()".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[test]
    fn encodes_like_browser() {
        assert_eq!(super::encode("Fotos/Urlaub 2026"), "Fotos%2FUrlaub%202026");
        assert_eq!(super::encode("Grüße"), "Gr%C3%BC%C3%9Fe");
    }
}
