# DropCraft

Eigener Dropbox-Klon: ein Rust-Server mit getrenntem Speicher pro Account, dazu Apps für Windows (fertig) und Android/iOS (geplant).

## Aufbau

```
server/          Rust-Backend (axum + SQLite), speichert Dateien als Blobs pro Account
deploy/          docker-compose + Caddy (automatisches HTTPS) + Setup-Skript für den VPS
client/core/     Sync-Engine + API-Client (Rust), später auch für die Handy-App
client/desktop/  Windows-App (egui): Sync-Ordner, Taskleisten-Symbol, Autostart
```

## Windows-App bauen und starten

Voraussetzung: Rust (https://rustup.rs) mit den Visual-Studio-Buildtools.

```powershell
cd client
cargo build --release -p dropcraft-desktop
.\target\release\DropCraft.exe
```

Die fertige Datei `client\target\release\DropCraft.exe` kannst du an einen beliebigen Ort kopieren.
Beim ersten Start fragt sie nach Server, Benutzername, Passwort und Sync-Ordner.

- **Fenster schließen** = in die Taskleiste minimieren. Beenden über das Taskleisten-Symbol (Rechtsklick).
- Einstellungen und Sync-Stand liegen in `%APPDATA%\DropCraft\`.
- Ignoriert werden u. a. `~$…`-Sperrdateien von Office, `*.tmp`, `desktop.ini`, `Thumbs.db`.
- Wird eine Datei auf zwei Geräten gleichzeitig geändert, bleibt die lokale Version als
  „Name (Konflikt Gerät Datum).ext“ erhalten.

Tests des Sync-Kerns (startet einen echten Server, braucht vorher `cargo build --release` in `server/`):

```bash
cd client && cargo test -p dropcraft-core
```

Jeder Account hat einen eigenen Ordner `data/users/<id>/blobs/` und ein eigenes Speicherlimit.
Die Datenbank merkt sich Pfade, Größe, SHA-256 und eine Revisionsnummer pro Datei.
Gelöschte Dateien bleiben als „Grabstein“ in der Datenbank, damit andere Geräte die Löschung beim Sync mitbekommen.

## Auf dem VPS installieren

```bash
# als root auf dem Server
apt-get update && apt-get install -y git
git clone <dein-repo> /opt/dropcraft      # oder per scp hochladen
cd /opt/dropcraft/deploy
./setup.sh                    # ohne Domain -> https://<ip-mit-strichen>.sslip.io
./setup.sh cloud.example.de   # mit eigener Domain (A-Record auf die Server-IP)

# ersten Admin anlegen (fragt nach dem Passwort)
docker compose exec -it dropcraft dropcraft-server adduser justin 100 --admin
```

Updates: `git pull && docker compose up -d --build`
Logs: `docker compose logs -f dropcraft`
Backup: Ordner `deploy/data/` sichern (enthält Datenbank und alle Dateien).

## Einstellungen (`deploy/.env`)

| Variable | Bedeutung | Standard |
|---|---|---|
| `DROPCRAFT_DOMAIN` | Domain für HTTPS | – |
| `DROPCRAFT_DEFAULT_QUOTA_GB` | Speicherlimit für neue Accounts | 20 |
| `DROPCRAFT_OPEN_REGISTRATION` | Jeder darf sich registrieren | false |

## API

Alle Endpunkte unter `/api`. Authentifizierung mit `Authorization: Bearer <token>`.

| Methode | Pfad | Beschreibung |
|---|---|---|
| POST | `/auth/login` | `{username, password, device_name}` → `{token}` |
| POST | `/auth/logout` | Token ungültig machen |
| POST | `/auth/password` | `{old_password, new_password}`, meldet andere Geräte ab |
| GET | `/me` | Name, Speicherlimit, belegter Speicher, aktueller Sync-Cursor |
| GET | `/files/list?path=Ordner` | Inhalt eines Ordners |
| PUT | `/files/upload?path=a/b.txt&mtime=<ms>&base_rev=<rev>` | Datei hochladen (Rohdaten im Body). Elternordner werden angelegt. `base_rev` = Konfliktschutz, `0` = darf nicht existieren |
| GET | `/files/download?path=a/b.txt` | Datei herunterladen (Header `x-sha256`, `x-rev`) |
| POST | `/files/mkdir` | `{path}` |
| POST | `/files/delete` | `{path}`, Ordner rekursiv |
| POST | `/files/move` | `{from, to}`, auch Umbenennen |
| GET | `/sync/changes?cursor=<n>&limit=1000` | Alle Änderungen seit `cursor`. `cursor=0` liefert den kompletten Stand |
| GET/POST | `/admin/users` | Accounts auflisten / anlegen `{username, password, quota_gb}` |
| PATCH/DELETE | `/admin/users/{id}` | Quota/Passwort ändern / Account löschen |

### So synchronisiert ein Client

1. Beim ersten Start `GET /sync/changes?cursor=0` (mit `has_more` weiterblättern), alles herunterladen, `cursor` merken.
2. Danach regelmäßig `GET /sync/changes?cursor=<gemerkt>` → nur die Änderungen anwenden.
3. Lokale Änderungen mit `PUT /files/upload?...&base_rev=<rev, die der Client kennt>` hochladen.
   Bei `409 Conflict` hat ein anderes Gerät die Datei geändert → lokale Version als „Konflikt-Kopie“ speichern.

## Entwickeln & testen

```bash
cd server
cargo run --release                          # Server auf :8080, Daten in ./data
DROPCRAFT_PASSWORD=geheim1234 cargo run --release -- adduser justin 10 --admin
./tests/api_test.sh                          # End-to-End-Tests (braucht curl + jq)
```
