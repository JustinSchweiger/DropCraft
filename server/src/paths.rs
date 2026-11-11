//! Normalisierung von Nutzerpfaden. Pfade werden immer relativ zum
//! Speicher des jeweiligen Accounts gespeichert, z. B. "Fotos/urlaub.jpg".
//! Der Wurzelordner ist der leere String "".

use crate::error::ApiError;

const MAX_PATH_LEN: usize = 1024;
const MAX_NAME_LEN: usize = 255;

pub fn normalize(raw: &str) -> Result<String, ApiError> {
    let raw = raw.replace('\\', "/");
    let mut parts = Vec::new();
    for part in raw.split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." {
            return Err(ApiError::BadRequest("Ungültiger Pfad".into()));
        }
        if part.len() > MAX_NAME_LEN || part.chars().any(|c| c.is_control()) {
            return Err(ApiError::BadRequest("Ungültiger Dateiname".into()));
        }
        parts.push(part);
    }
    let path = parts.join("/");
    if path.len() > MAX_PATH_LEN {
        return Err(ApiError::BadRequest("Pfad zu lang".into()));
    }
    Ok(path)
}

/// Wie `normalize`, aber der Wurzelordner ist nicht erlaubt.
pub fn normalize_non_root(raw: &str) -> Result<String, ApiError> {
    let p = normalize(raw)?;
    if p.is_empty() {
        return Err(ApiError::BadRequest("Wurzelordner nicht erlaubt".into()));
    }
    Ok(p)
}

#[allow(dead_code)]
pub fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(i) => &path[..i],
        None => "",
    }
}

/// Alle Vorfahren eines Pfades, vom obersten zum direkten Elternordner.
/// "a/b/c.txt" -> ["a", "a/b"]
pub fn ancestors(path: &str) -> Vec<&str> {
    let mut out = Vec::new();
    for (i, c) in path.char_indices() {
        if c == '/' {
            out.push(&path[..i]);
        }
    }
    out
}

/// Escaping für SQL LIKE mit ESCAPE '\'.
pub fn like_prefix(dir: &str) -> String {
    let mut s = String::with_capacity(dir.len() + 2);
    for c in dir.chars() {
        if c == '%' || c == '_' || c == '\\' {
            s.push('\\');
        }
        s.push(c);
    }
    s.push_str("/%");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes() {
        assert_eq!(normalize("/a//b/").unwrap(), "a/b");
        assert_eq!(normalize("a\\b\\c.txt").unwrap(), "a/b/c.txt");
        assert_eq!(normalize("").unwrap(), "");
        assert!(normalize("a/../b").is_err());
        assert!(normalize("./a").is_err());
        assert!(normalize_non_root("/").is_err());
    }

    #[test]
    fn ancestors_work() {
        assert_eq!(ancestors("a/b/c.txt"), vec!["a", "a/b"]);
        assert!(ancestors("c.txt").is_empty());
        assert_eq!(parent("a/b/c"), "a/b");
        assert_eq!(parent("c"), "");
    }

    #[test]
    fn like_escapes() {
        assert_eq!(like_prefix("100%_x"), "100\\%\\_x/%");
    }
}
