//! Kleines Protokoll in `%APPDATA%\DropCraft\app.log`: Start (Argumente),
//! wann und warum das Popup erscheint, Meldungen. Hilft bei Fragen wie
//! „warum war das Fenster beim Windows-Start offen?“. Höchstens ~256 KB,
//! danach wird die alte Datei zu `app.log.old`.

pub fn line(msg: &str) {
    use std::io::Write;
    let dir = crate::config::app_dir();
    let path = dir.join("app.log");
    if std::fs::metadata(&path).map(|m| m.len() > 256 * 1024).unwrap_or(false) {
        let _ = std::fs::rename(&path, dir.join("app.log.old"));
    }
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&path) {
        // Eine Zeile in einem Stück schreiben, sonst mischen sich Prozesse.
        let text = format!("{} [{}] {msg}\n", chrono::Local::now().format("%Y-%m-%d %H:%M:%S%.3f"), std::process::id());
        let _ = f.write_all(text.as_bytes());
    }
}
