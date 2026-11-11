//! End-to-End: zwei Geräte gleichen über einen echten Server ab.
//! Braucht die Server-Binary (DROPCRAFT_SERVER_BIN oder ../../server/target/release).

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    thread::sleep,
    time::{Duration, SystemTime},
};

use dropcraft_core::{Api, EventKind, State, SyncEngine};

struct Server {
    child: Child,
    url: String,
    _data: tempfile::TempDir,
}
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

fn server_bin() -> PathBuf {
    std::env::var("DROPCRAFT_SERVER_BIN").map(PathBuf::from).unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server/target/release/dropcraft-server")
    })
}

fn start_server(port: u16) -> Server {
    let data = tempfile::tempdir().unwrap();
    let bin = server_bin();
    let status = Command::new(&bin)
        .args(["adduser", "justin", "1", "--admin"])
        .env("DROPCRAFT_DATA", data.path())
        .env("DROPCRAFT_PASSWORD", "sehrgeheim123")
        .status()
        .expect("Server-Binary fehlt – erst server bauen");
    assert!(status.success());
    let child = Command::new(&bin)
        .env("DROPCRAFT_DATA", data.path())
        .env("DROPCRAFT_BIND", format!("127.0.0.1:{port}"))
        .env("RUST_LOG", "warn")
        .spawn()
        .unwrap();
    let url = format!("http://127.0.0.1:{port}");
    let api = Api::new(&url, None).unwrap();
    for _ in 0..100 {
        if api.health().is_ok() {
            break;
        }
        sleep(Duration::from_millis(50));
    }
    Server { child, url, _data: data }
}

struct Device {
    engine: SyncEngine,
    dir: tempfile::TempDir,
    _db: tempfile::TempDir,
}

fn device(url: &str, name: &str) -> Device {
    let api = Api::login(url, "justin", "sehrgeheim123", name).unwrap();
    let dir = tempfile::tempdir().unwrap();
    let db = tempfile::tempdir().unwrap();
    let state = State::open(&db.path().join("state.db")).unwrap();
    Device { engine: SyncEngine::new(api, dir.path().to_path_buf(), state, name.into()), dir, _db: db }
}

/// Schreibt eine Datei und setzt die Änderungszeit etwas zurück,
/// damit sie nicht als "wird gerade geschrieben" übersprungen wird.
fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    let t = SystemTime::now() - Duration::from_secs(5);
    filetime::set_file_mtime(&p, filetime::FileTime::from_system_time(t)).unwrap();
    sleep(Duration::from_millis(5));
}

fn read(root: &Path, rel: &str) -> Option<Vec<u8>> {
    fs::read(root.join(rel)).ok()
}

fn names(root: &Path, rel: &str) -> Vec<String> {
    let mut v: Vec<String> = fs::read_dir(root.join(rel))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    v.sort();
    v
}

#[test]
fn two_devices_stay_in_sync() {
    let srv = start_server(18600 + (std::process::id() % 300) as u16);
    let mut a = device(&srv.url, "PC");
    let mut b = device(&srv.url, "Handy");
    let (ra, rb) = (a.dir.path().to_path_buf(), b.dir.path().to_path_buf());

    // 1. Neue Dateien und Ordner von A kommen bei B an.
    write(&ra, "Dokumente/Brief.txt", b"Hallo");
    write(&ra, "Fotos/2026/bild.jpg", &vec![7u8; 300_000]);
    fs::create_dir_all(ra.join("Leer")).unwrap();
    write(&ra, "~$Brief.docx", b"Office-Sperrdatei");
    write(&ra, "desktop.ini", b"x");
    let r = a.engine.sync_once().unwrap();
    assert_eq!(r.uploaded, 2, "{r:?}");
    let r = b.engine.sync_once().unwrap();
    assert_eq!(r.downloaded, 2, "{r:?}");
    // Fortschritt: Bytes gezählt, Warteschlange danach leer.
    {
        let pa = a.engine.progress();
        let pa = pa.lock().unwrap();
        assert_eq!(pa.bytes_up, 300_005, "hochgeladene Bytes");
        assert_eq!(pa.files_done, 2);
        assert!(!pa.is_active());
        let pb = b.engine.progress();
        let pb = pb.lock().unwrap();
        assert_eq!(pb.bytes_down, 300_005, "heruntergeladene Bytes");
        assert!(pb.queue.is_empty() && pb.current.is_none());
    }
    assert_eq!(read(&rb, "Dokumente/Brief.txt").unwrap(), b"Hallo");
    assert_eq!(read(&rb, "Fotos/2026/bild.jpg").unwrap().len(), 300_000);
    assert!(rb.join("Leer").is_dir(), "leerer Ordner fehlt");
    assert!(!rb.join("~$Brief.docx").exists(), "ignorierte Datei wurde synchronisiert");
    assert!(!rb.join("desktop.ini").exists());

    // Zweiter Durchgang ohne Änderungen tut nichts.
    assert!(!a.engine.sync_once().unwrap().changed());
    assert!(!b.engine.sync_once().unwrap().changed());

    // 2. Änderung auf B kommt bei A an.
    write(&rb, "Dokumente/Brief.txt", b"Hallo, geaendert");
    b.engine.sync_once().unwrap();
    a.engine.sync_once().unwrap();
    assert_eq!(read(&ra, "Dokumente/Brief.txt").unwrap(), b"Hallo, geaendert");

    // 3. Löschen auf A löscht auf B.
    fs::remove_file(ra.join("Fotos/2026/bild.jpg")).unwrap();
    a.engine.sync_once().unwrap();
    b.engine.sync_once().unwrap();
    assert!(!rb.join("Fotos/2026/bild.jpg").exists());

    // 4. Umbenennen auf A.
    fs::rename(ra.join("Dokumente/Brief.txt"), ra.join("Dokumente/Brief final.txt")).unwrap();
    a.engine.sync_once().unwrap();
    b.engine.sync_once().unwrap();
    assert_eq!(names(&rb, "Dokumente"), vec!["Brief final.txt"]);

    // 5. Konflikt: beide ändern dieselbe Datei.
    write(&ra, "Dokumente/Brief final.txt", b"Version PC");
    write(&rb, "Dokumente/Brief final.txt", b"Version Handy");
    a.engine.sync_once().unwrap();
    let r = b.engine.sync_once().unwrap();
    assert_eq!(r.conflicts, 1, "{r:?}");
    assert!(r.events.iter().any(|e| e.kind == EventKind::Conflict));
    a.engine.sync_once().unwrap();
    let na = names(&ra, "Dokumente");
    let nb = names(&rb, "Dokumente");
    assert_eq!(na, nb, "beide Geräte sollen dieselben Dateien haben");
    assert_eq!(na.len(), 2, "{na:?}");
    assert_eq!(read(&ra, "Dokumente/Brief final.txt").unwrap(), b"Version PC");
    let copy = na.iter().find(|n| n.contains("Konflikt Handy")).expect("Konflikt-Kopie fehlt");
    assert_eq!(read(&ra, &format!("Dokumente/{copy}")).unwrap(), b"Version Handy");

    // 6. Ordner mit Inhalt löschen.
    fs::remove_dir_all(rb.join("Dokumente")).unwrap();
    b.engine.sync_once().unwrap();
    a.engine.sync_once().unwrap();
    assert!(!ra.join("Dokumente").exists());

    // 7. Lokal gelöscht, aber auf dem anderen Gerät inzwischen geändert:
    //    die Änderung gewinnt (nichts geht verloren).
    write(&ra, "wichtig.txt", b"v1");
    a.engine.sync_once().unwrap();
    b.engine.sync_once().unwrap();
    write(&rb, "wichtig.txt", b"v2 vom Handy");
    b.engine.sync_once().unwrap();
    fs::remove_file(ra.join("wichtig.txt")).unwrap();
    a.engine.sync_once().unwrap();
    assert_eq!(read(&ra, "wichtig.txt").unwrap(), b"v2 vom Handy");

    // 8. Speicherlimit (1 GB) -> Fehler-Ereignis statt Abbruch.
    let big = fs::File::create(ra.join("riesig.bin")).unwrap();
    big.set_len(1_200_000_000).unwrap();
    drop(big);
    let t = SystemTime::now() - Duration::from_secs(5);
    filetime::set_file_mtime(ra.join("riesig.bin"), filetime::FileTime::from_system_time(t)).unwrap();
    let r = a.engine.sync_once().unwrap();
    assert!(r.events.iter().any(|e| e.kind == EventKind::Error && e.detail.contains("Speicherplatz")), "{r:?}");
    fs::remove_file(ra.join("riesig.bin")).unwrap();

    // 9. Fehlender Sync-Ordner darf nichts auf dem Server löschen.
    let moved = ra.with_extension("weg");
    fs::rename(&ra, &moved).unwrap();
    assert!(a.engine.sync_once().is_err());
    fs::rename(&moved, &ra).unwrap();
    b.engine.sync_once().unwrap();
    assert_eq!(read(&rb, "wichtig.txt").unwrap(), b"v2 vom Handy");
}
