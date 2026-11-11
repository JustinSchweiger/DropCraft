//! Dateien bei Bedarf: Gerät B bekommt Platzhalter statt Downloads.
//! Die Platzhalter werden hier simuliert (Datei mit richtiger Größe, ohne Inhalt).

use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Arc, Mutex},
    thread::sleep,
    time::{Duration, SystemTime},
};

use dropcraft_core::{Api, LocalHooks, RemoteEntry, State, SyncEngine};

#[derive(Clone, Default)]
struct Fake {
    online: Arc<Mutex<HashSet<PathBuf>>>,
    pinned: Arc<Mutex<HashSet<PathBuf>>>,
    free: Arc<Mutex<HashSet<PathBuf>>>,
}

fn set_mtime(p: &Path, ms: i64) {
    let ft = filetime::FileTime::from_unix_time(ms / 1000, ((ms % 1000) * 1_000_000) as u32);
    filetime::set_file_mtime(p, ft).unwrap();
}

impl LocalHooks for Fake {
    fn is_online_only(&self, l: &Path) -> bool {
        self.online.lock().unwrap().contains(l)
    }
    fn wants_local(&self, l: &Path) -> bool {
        let pinned = self.pinned.lock().unwrap();
        l.ancestors().any(|a| pinned.contains(a))
    }
    fn wants_free(&self, l: &Path) -> bool {
        let free = self.free.lock().unwrap();
        l.ancestors().any(|a| free.contains(a))
    }
    fn create_placeholder(&self, l: &Path, e: &RemoteEntry) -> std::io::Result<bool> {
        let f = fs::File::create(l)?;
        f.set_len(e.size as u64)?;
        drop(f);
        set_mtime(l, e.modified_at);
        self.online.lock().unwrap().insert(l.to_path_buf());
        Ok(true)
    }
    fn update_placeholder(&self, l: &Path, e: &RemoteEntry) -> std::io::Result<bool> {
        self.create_placeholder(l, e)
    }
    fn free_space(&self, l: &Path) -> std::io::Result<()> {
        let len = fs::metadata(l)?.len();
        let m = fs::metadata(l)?.modified()?;
        let f = fs::OpenOptions::new().write(true).truncate(true).open(l)?;
        f.set_len(len)?;
        drop(f);
        filetime::set_file_mtime(l, filetime::FileTime::from_system_time(m))?;
        self.online.lock().unwrap().insert(l.to_path_buf());
        Ok(())
    }
    fn mark_synced(&self, l: &Path, _is_dir: bool) {
        self.online.lock().unwrap().remove(l);
    }
}

struct Server(Child, tempfile::TempDir);
impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
    }
}

fn server(port: u16) -> (Server, String) {
    let data = tempfile::tempdir().unwrap();
    let bin = std::env::var("DROPCRAFT_SERVER_BIN").map(PathBuf::from).unwrap_or_else(|_| {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../server/target/release/dropcraft-server")
    });
    assert!(Command::new(&bin)
        .args(["adduser", "justin", "1", "--admin"])
        .env("DROPCRAFT_DATA", data.path())
        .env("DROPCRAFT_PASSWORD", "sehrgeheim123")
        .status()
        .unwrap()
        .success());
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
    (Server(child, data), url)
}

fn engine(url: &str, name: &str, dir: &Path, db: &Path) -> SyncEngine {
    let api = Api::login(url, "justin", "sehrgeheim123", name).unwrap();
    SyncEngine::new(api, dir.to_path_buf(), State::open(&db.join("s.db")).unwrap(), name.into())
}

fn write(root: &Path, rel: &str, content: &[u8]) {
    let p = root.join(rel);
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(&p, content).unwrap();
    filetime::set_file_mtime(&p, filetime::FileTime::from_system_time(SystemTime::now() - Duration::from_secs(5))).unwrap();
    sleep(Duration::from_millis(5));
}

#[test]
fn placeholders_pin_and_free() {
    let (_srv, url) = server(18900 + (std::process::id() % 90) as u16);
    let (da, db_a, dbb, db_b) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (ra, rb) = (da.path().to_path_buf(), dbb.path().to_path_buf());
    let mut a = engine(&url, "PC", &ra, db_a.path());
    let mut b = engine(&url, "Laptop", &rb, db_b.path());
    let fake = Fake::default();
    b.set_hooks(Box::new(fake.clone()));

    write(&ra, "Fotos/a.jpg", &vec![1u8; 300_000]);
    write(&ra, "Doks/b.txt", b"version 1");
    a.sync_once().unwrap();

    // 1. B bekommt nur Platzhalter, lädt nichts herunter.
    b.sync_once().unwrap();
    assert_eq!(b.progress().lock().unwrap().bytes_down, 0, "nichts sollte geladen werden");
    assert_eq!(fs::metadata(rb.join("Fotos/a.jpg")).unwrap().len(), 300_000);
    assert!(fake.is_online_only(&rb.join("Fotos/a.jpg")));
    // Platzhalter werden nie hochgeladen.
    let r = b.sync_once().unwrap();
    assert_eq!(r.uploaded, 0, "{r:?}");

    // 2. Ordner anpinnen -> Inhalt wird geladen.
    fake.pinned.lock().unwrap().insert(rb.join("Doks"));
    b.sync_once().unwrap();
    assert_eq!(fs::read(rb.join("Doks/b.txt")).unwrap(), b"version 1");
    assert!(!fake.is_online_only(&rb.join("Doks/b.txt")));
    assert!(fake.is_online_only(&rb.join("Fotos/a.jpg")), "nicht angepinnte Datei bleibt online");

    // 3. Änderungen auf A: angepinnte Datei wird geladen, andere bleibt Platzhalter.
    write(&ra, "Doks/b.txt", b"version 2, laenger");
    write(&ra, "Fotos/a.jpg", &vec![2u8; 123_456]);
    a.sync_once().unwrap();
    let down_before = b.progress().lock().unwrap().bytes_down;
    b.sync_once().unwrap();
    assert_eq!(fs::read(rb.join("Doks/b.txt")).unwrap(), b"version 2, laenger");
    assert_eq!(fs::metadata(rb.join("Fotos/a.jpg")).unwrap().len(), 123_456, "Platzhalter mit neuer Größe");
    assert!(fake.is_online_only(&rb.join("Fotos/a.jpg")));
    assert_eq!(b.progress().lock().unwrap().bytes_down - down_before, 18, "nur b.txt geladen");

    // 4. Speicher freigeben: Inhalt weg, Datei bleibt, nichts wird hochgeladen.
    fake.pinned.lock().unwrap().clear();
    fake.free.lock().unwrap().insert(rb.join("Doks"));
    let r = b.sync_once().unwrap();
    assert!(fake.is_online_only(&rb.join("Doks/b.txt")));
    assert_eq!(r.uploaded, 0);
    let r = b.sync_once().unwrap();
    assert_eq!(r.uploaded, 0, "freigegebene Datei darf nicht hochgeladen werden");
    assert_eq!(fs::read(ra.join("Doks/b.txt")).unwrap(), b"version 2, laenger", "Server-Version unverändert");

    // 5. Platzhalter löschen löscht auf dem Server.
    fs::remove_file(rb.join("Fotos/a.jpg")).unwrap();
    b.sync_once().unwrap();
    a.sync_once().unwrap();
    assert!(!ra.join("Fotos/a.jpg").exists());

    // 6. Neue lokale Datei auf B geht ganz normal hoch.
    write(&rb, "neu.txt", b"vom Laptop");
    b.sync_once().unwrap();
    a.sync_once().unwrap();
    assert_eq!(fs::read(ra.join("neu.txt")).unwrap(), b"vom Laptop");
    assert!(!fake.is_online_only(&rb.join("neu.txt")));
}
