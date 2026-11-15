# DropCraft

Eigener Dropbox-Klon: ein Rust-Server mit getrenntem Speicher pro Account, dazu eine Windows-App (mehrere Konten, Taskleisten-Popup) und später Android/iOS.

## Aufbau

```
server/          Rust-Backend (axum + SQLite), speichert Dateien als Blobs pro Account
deploy/          docker-compose + Caddy (automatisches HTTPS) + Setup-Skript für den VPS
client/core/     Sync-Engine + API-Client (Rust), später auch für die Handy-App
client/desktop/  Windows-App (egui): Sync-Ordner, Taskleisten-Symbol, Autostart
```

## Windows-App bauen und starten

Voraussetzung: Rust (https://rustup.rs, mindestens 1.93 – `rustup update`) mit den Visual-Studio-Buildtools.

```powershell
cd client
cargo build --release -p dropcraft-desktop                         # Standard: alles wird heruntergeladen
cargo build --release -p dropcraft-desktop --features cloud-files  # mit Platzhaltern + Status-Spalte (Test)
.\target\release\DropCraft.exe
```

### Dateien bei Bedarf (`--features cloud-files`, noch experimentell)

- Der Sync-Ordner wird bei Windows als Cloud-Ordner angemeldet und erscheint links im Explorer.
- Neue Dateien vom Server erscheinen als **Platzhalter** (Wolke) und werden erst beim Öffnen geladen.
- **Status-Spalte** im Explorer: Wolke = nur online, Haken = auf diesem Gerät, voller Haken = immer verfügbar.
- Rechtsklick › **„Immer auf diesem Gerät behalten“** lädt einen Ordner komplett und hält ihn aktuell,
  **„Speicherplatz freigeben“** macht ihn wieder zu Platzhaltern.
- Beim Entfernen eines Kontos werden vorher alle Platzhalter heruntergeladen.

Die fertige Datei `client\target\release\DropCraft.exe` kannst du an einen beliebigen Ort kopieren.

### Bedienung

- **Popup über der Taskleiste:** Klick aufs DropCraft-Symbol öffnet das Fenster, Wegklicken oder Esc schließt es.
  Rechtsklick aufs Symbol: Öffnen, Pausieren, Beenden.
- **Live-Statistik:** aktuelle Datei mit Fortschritt, Geschwindigkeit, verbleibende Dateien/Größe, Restzeit, Warteschlange.
- **Mehrere Konten:** Konto › „Konto hinzufügen …“. Jedes Konto hat eigenen Server, eigene Anmeldung und eigenen
  Sync-Ordner (z. B. `DropCraft` und `DropCraft ocv`). Ordner dürfen sich nicht überschneiden. Oben im Popup
  zwischen den Konten wechseln.
- **Benutzer verwalten** (nur für Admin-Konten): Konto › „Benutzer verwalten …“ – Name, Passwort und Speicher in GB
  eingeben, fertig. Speicher ändern, Passwort zurücksetzen, Benutzer löschen. Neue Benutzer sind nie Admins.
- **Ordnersymbol:** Sync-Ordner bekommen im Explorer ein eigenes Symbol (über `desktop.ini`).
- **Taskleisten-Symbol:** dreht sich beim Synchronisieren; grau = pausiert, gelb = Problem. Tooltip mit Restmenge.
- **Freigabe-Optionen:** pro Link Upload für Gäste, Passwort, Ablaufdatum und Benachrichtigungen (Windows-Meldung,
  wenn andere dort Dateien hinzufügen oder löschen). Die Seite zeigt Bilder/Videos als Galerie mit Vorschau.
- **Einstellungen:** Bandbreitenlimit für Upload/Download; „Pausieren“ stoppt laufende Übertragungen sofort.
- **Freigaben:** Rechtsklick auf Ordner/Datei im Sync-Ordner › „DropCraft-Link kopieren“ (Windows 11: unter
  „Weitere Optionen anzeigen“) oder Popup › Konto › „Freigaben …“. Der Link öffnet eine Webseite mit dem Inhalt,
  Einzel-Downloads und „Alles als ZIP“ – ohne DropCraft. Links lassen sich dort auch wieder beenden.
- Einstellungen und Sync-Stand liegen in `%APPDATA%\DropCraft\` (pro Konto unter `accounts\<id>\`).
- Ignoriert werden u. a. `~$…`-Sperrdateien von Office, `*.tmp`, `desktop.ini`, `Thumbs.db`.
- Wird eine Datei auf zwei Geräten gleichzeitig geändert, bleibt die lokale Version als
  „Name (Konflikt Gerät Datum).ext“ erhalten.

### Weiterer Server (z. B. nur für „ocv“)

Wie oben „Auf dem VPS installieren“, dann nur diesen einen Account anlegen. Ohne `--admin` ist er ein normaler
Nutzer; Speicher änderst du dann per `docker compose exec dropcraft dropcraft-server …` bzw. legst den Account neu an.

Tests des Sync-Kerns (startet einen echten Server, braucht vorher `cargo build --release` in `server/`):

```bash
cd client && cargo test -p dropcraft-core
```

Jeder Account hat einen eigenen Ordner `data/users/<id>/blobs/` und ein eigenes Speicherlimit.
Die Datenbank merkt sich Pfade, Größe, SHA-256 und eine Revisionsnummer pro Datei.
Gelöschte Dateien bleiben als „Grabstein“ in der Datenbank, damit andere Geräte die Löschung beim Sync mitbekommen.

## n8n auf demselben Server

`deploy/docker-compose.yml` startet zusätzlich n8n unter `https://n8n.dropcraft.tech` (anderer Name: `N8N_DOMAIN=…` in `deploy/.env`).
Voraussetzung: DNS-A-Eintrag `n8n` auf die Server-IP. Der Schlüssel `N8N_ENCRYPTION_KEY` steht in `deploy/.env`,
wird beim ersten Update automatisch erzeugt und darf danach nie geändert werden (sonst sind gespeicherte Zugangsdaten unlesbar).
Update von n8n: `cd /opt/dropcraft/deploy && docker compose pull n8n && docker compose up -d n8n`.

## Mac-App bauen und starten

Voraussetzungen: Xcode (oder nur die Command Line Tools) und Rust (`curl https://sh.rustup.rs -sSf | sh`).

```bash
git clone https://github.com/JustinSchweiger/DropCraft.git && cd DropCraft
bash client/desktop/macos/bundle.sh
open client/target/release/DropCraft.app      # oder nach /Programme ziehen
```

- Die App lebt in der **Menüleiste** (kein Dock-Symbol). Klick aufs Symbol öffnet das Popup, Rechtsklick zeigt das Menü.
- Kein Apple-Entwicklerkonto nötig: Das Skript signiert ad hoc. Selbst gebaut startet die App direkt.
- Fertig gebaut: GitHub → Actions → letzter CI-Lauf → `DropCraft-macOS`. Beim ersten Start Rechtsklick → „Öffnen“
  (oder `xattr -dr com.apple.quarantine DropCraft.app`).
- „Bei der Anmeldung starten“ legt einen LaunchAgent an (`~/Library/LaunchAgents/tech.dropcraft.app.plist`).
- Noch nicht auf dem Mac: Platzhalter-Dateien („Dateien bei Bedarf“) und der Finder-Rechtsklick. Bis dahin werden alle Dateien geladen.

## iPhone-/iPad-App

Native SwiftUI-App in `ios/`: Anmelden (mehrere Konten), Ordner durchblättern, Dateien öffnen (Vorschau),
hochladen aus Fotos und Dateien, Links kopieren, Freigaben einstellen, Benutzerverwaltung für Admins.

### Mit Xcode installieren (kostenlos, Apple-ID genügt)

```bash
brew install xcodegen
cd ios && xcodegen generate && open DropCraft.xcodeproj
```

1. Xcode → Einstellungen → Accounts: Apple-ID hinzufügen.
2. Ziel „DropCraft“ → Signing & Capabilities: Team = „(Personal Team)“. Meldet Xcode, dass die
   Kennung vergeben ist, `tech.dropcraft.ios` durch etwas Eigenes ersetzen (z. B. `de.justin.dropcraft`).
3. iPhone per Kabel anschließen, oben als Ziel wählen, ▶ drücken.
4. Auf dem iPhone: Einstellungen → Allgemein → VPN und Geräteverwaltung → Entwickler vertrauen.
   Ggf. zusätzlich Einstellungen → Datenschutz & Sicherheit → Entwicklermodus einschalten.

Mit kostenloser Apple-ID läuft die Signatur nach **7 Tagen** ab (Daten bleiben erhalten): dann einfach erneut ▶.

### Ohne Kabel erneuern: SideStore / AltStore / Sideloadly

Die CI baut eine unsignierte `DropCraft.ipa` (GitHub → Actions → CI → `DropCraft-iOS`). Diese Werkzeuge
signieren sie mit deiner Apple-ID und erneuern sie automatisch vor Ablauf der 7 Tage.

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

## Automatisches Deployment (GitHub Actions)

- **CI** (`.github/workflows/ci.yml`): bei jedem Push Server-Tests, Sync-Kern-Tests und Windows-Builds
  (mit und ohne `cloud-files`). Die fertigen `DropCraft.exe` liegen als Artefakte am Lauf.
- **Deploy** (`.github/workflows/deploy.yml`): nach erfolgreicher CI auf `main` wird der Server aktualisiert
  und die Windows-App unter `https://<domain>/download/windows` bereitgestellt (Startseite).

Einmalig einrichten – auf dem Server:

```bash
ssh-keygen -t ed25519 -N "" -C github-deploy -f ~/.ssh/github_deploy
cat ~/.ssh/github_deploy.pub >> ~/.ssh/authorized_keys
cat ~/.ssh/github_deploy        # privaten Schlüssel kopieren
```

Dann im Repo unter **Settings › Secrets and variables › Actions** anlegen:
`DEPLOY_HOST` = `dropcraft.tech`, `DEPLOY_SSH_KEY` = der kopierte private Schlüssel (komplett, mit BEGIN/END-Zeilen).

## Einstellungen (`deploy/.env`)

| Variable | Bedeutung | Standard |
|---|---|---|
| `DROPCRAFT_DOMAIN` | Domain für HTTPS | – |
| `DROPCRAFT_DEFAULT_QUOTA_GB` | Speicherlimit für neue Accounts | 20 |
| `DROPCRAFT_OPEN_REGISTRATION` | Jeder darf sich registrieren | false |
| `DROPCRAFT_ALIASES` | Weitere Adressen (z. B. alte Domain) | – |

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
| GET/POST | `/shares` | Freigaben auflisten / anlegen `{path}` → `{token}`; öffentlich unter `/s/{token}` |
| DELETE | `/shares/{token}` | Freigabe beenden |
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

## Musik-Check (https://dropcraft.tech/musik/)

Kleine Webseite zum Prüfen zusammengeschnittener Musikdateien (z. B. für den Fasching):
Aufbau mit Schnitten und Tempo, Lautheit (LUFS nach EBU R128) je Teil, Übersteuerungen,
Tipps und eine angeglichene WAV-Fassung zum Herunterladen. Die Analyse läuft komplett im
Browser, die Musik wird nicht hochgeladen. Dateien: `deploy/musik/`, ausgeliefert von Caddy.
