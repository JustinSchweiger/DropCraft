//! Windows-Benachrichtigungen bei Änderungen in freigegebenen Ordnern.

use std::collections::BTreeMap;

use dropcraft_core::{Api, Event, EventKind};

/// Fasst Änderungen anderer (Gäste, andere Geräte) in Freigaben mit
/// eingeschalteter Benachrichtigung zusammen und zeigt je Freigabe eine Meldung.
pub fn shared_folder_changes(api: &Api, own_device: &str, events: &[Event]) {
    let relevant: Vec<&Event> = events
        .iter()
        .filter(|e| matches!(e.kind, EventKind::Downloaded | EventKind::NewOnline | EventKind::DeletedLocal))
        .filter(|e| e.by.as_deref().map(|b| b != own_device).unwrap_or(false))
        .collect();
    if relevant.is_empty() {
        return;
    }
    let Ok(shares) = api.list_shares() else { return };
    // Freigabe-Pfad -> (neu, gelöscht, wer)
    let mut groups: BTreeMap<String, (usize, usize, Vec<String>)> = BTreeMap::new();
    for e in relevant {
        let Some(sh) = shares
            .iter()
            .filter(|s| s.notify && (e.path == s.path || e.path.starts_with(&format!("{}/", s.path))))
            .max_by_key(|s| s.path.len())
        else {
            continue;
        };
        let g = groups.entry(sh.path.clone()).or_default();
        if e.kind == EventKind::DeletedLocal {
            g.1 += 1;
        } else {
            g.0 += 1;
        }
        if let Some(b) = &e.by {
            if !g.2.contains(b) {
                g.2.push(b.clone());
            }
        }
    }
    for (path, (added, removed, who)) in groups {
        let name = path.rsplit('/').next().unwrap_or(&path).to_string();
        show(&format!("DropCraft · {name}"), &summary(added, removed, &who));
    }
}

pub fn summary(added: usize, removed: usize, who: &[String]) -> String {
    let mut parts = Vec::new();
    if added > 0 {
        parts.push(if added == 1 { "1 neue Datei".to_string() } else { format!("{added} neue Dateien") });
    }
    if removed > 0 {
        parts.push(if removed == 1 { "1 Datei gelöscht".to_string() } else { format!("{removed} Dateien gelöscht") });
    }
    let mut s = parts.join(", ");
    if !who.is_empty() {
        s.push_str(&format!(" · von {}", who.join(", ")));
    }
    s
}

pub fn show(title: &str, body: &str) {
    let res = notify_rust::Notification::new().appname("DropCraft").summary(title).body(body).show();
    if let Err(e) = res {
        log::warn!("Benachrichtigung fehlgeschlagen: {e}");
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn texts() {
        assert_eq!(super::summary(3, 0, &["Gast: Anna".into()]), "3 neue Dateien · von Gast: Anna");
        assert_eq!(super::summary(1, 2, &[]), "1 neue Datei, 2 Dateien gelöscht");
    }
}
