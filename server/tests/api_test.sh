#!/usr/bin/env bash
# End-to-End-Test der API. Startet einen frischen Server auf Port 18080.
set -euo pipefail
BIN=${BIN:-./target/release/dropcraft-server}
DATA=$(mktemp -d)
export DROPCRAFT_DATA=$DATA DROPCRAFT_BIND=127.0.0.1:18080 DROPCRAFT_DEFAULT_QUOTA_GB=0.001 RUST_LOG=warn
DROPCRAFT_PASSWORD=sehrgeheim123 $BIN adduser justin 0.001 --admin
$BIN & PID=$!
trap 'kill $PID 2>/dev/null; rm -rf "$DATA"' EXIT
B=http://127.0.0.1:18080/api
for i in $(seq 50); do curl -sf $B/health >/dev/null && break; sleep 0.1; done

pass=0; fail=0
check() { if [ "$2" = "$3" ]; then pass=$((pass+1)); echo "ok   $1"; else fail=$((fail+1)); echo "FAIL $1: erwartet '$3', bekommen '$2'"; fi; }
code() { curl -s -o /tmp/resp -w '%{http_code}' "$@"; }
J() { curl -s -H 'content-type: application/json' "$@"; }

# Accounts
check "freie Registrierung gesperrt" "$(code -H 'content-type: application/json' -X POST $B/auth/register -d '{"username":"mallory","password":"sehrgeheim123"}')" 403
check "falsches Passwort" "$(code -H 'content-type: application/json' -X POST $B/auth/login -d '{"username":"justin","password":"falsch12345"}')" 401
TA=$(J -X POST $B/auth/login -d '{"username":"justin","password":"sehrgeheim123","device_name":"test"}' | jq -r .token)
A=(-H "authorization: Bearer $TA")
check "Admin legt Account an" "$(code "${A[@]}" -H 'content-type: application/json' -X POST $B/admin/users -d '{"username":"anna","password":"annaspasswort1","quota_gb":0.001}')" 201
TB=$(J -X POST $B/auth/login -d '{"username":"anna","password":"annaspasswort1"}' | jq -r .token)
Bh=(-H "authorization: Bearer $TB")
check "Nicht-Admin darf keine Accounts anlegen" "$(code "${Bh[@]}" -H 'content-type: application/json' -X POST $B/admin/users -d '{"username":"x123","password":"xxxxxxxxxxxx"}')" 403
check "ohne Token" "$(code $B/me)" 401

# Dateien
echo "hallo welt" > /tmp/a.txt
check "Upload neu" "$(code "${A[@]}" -X PUT --data-binary @/tmp/a.txt "$B/files/upload?path=Fotos/2026/a.txt&mtime=1700000000000")" 201
check "Elternordner automatisch angelegt" "$(curl -s "${A[@]}" "$B/files/list?path=Fotos" | jq -r '.entries[0].name')" 2026
check "Liste Root" "$(curl -s "${A[@]}" "$B/files/list" | jq -c '[.entries[].name]')" '["Fotos"]'
check "Download Inhalt" "$(curl -s "${A[@]}" "$B/files/download?path=Fotos/2026/a.txt")" "hallo welt"
check "Größe belegt" "$(curl -s "${A[@]}" $B/me | jq .used_bytes)" 11
REV=$(curl -s "${A[@]}" "$B/files/list?path=Fotos/2026" | jq '.entries[0].rev')
check "Gleicher Inhalt -> 200, keine neue Rev" "$(curl -s "${A[@]}" -X PUT --data-binary @/tmp/a.txt "$B/files/upload?path=Fotos/2026/a.txt" | jq .rev)" "$REV"
echo "neu" > /tmp/b.txt
check "Konflikt bei falscher base_rev" "$(code "${A[@]}" -X PUT --data-binary @/tmp/b.txt "$B/files/upload?path=Fotos/2026/a.txt&base_rev=1")" 409
check "Update mit richtiger base_rev" "$(code "${A[@]}" -X PUT --data-binary @/tmp/b.txt "$B/files/upload?path=Fotos/2026/a.txt&base_rev=$REV")" 200
check "Quota nach Update" "$(curl -s "${A[@]}" $B/me | jq .used_bytes)" 4
check "Path traversal abgelehnt" "$(code "${A[@]}" -X PUT --data-binary @/tmp/b.txt "$B/files/upload?path=../../etc/x")" 400

# Trennung der Accounts
check "Anna sieht Justins Dateien nicht" "$(curl -s "${Bh[@]}" "$B/files/list" | jq '.entries|length')" 0
check "Anna kann Justins Datei nicht laden" "$(code "${Bh[@]}" "$B/files/download?path=Fotos/2026/a.txt")" 404

# Quota (1 MB)
head -c 2000000 /dev/urandom > /tmp/big.bin
check "Quota überschritten" "$(code "${Bh[@]}" -X PUT --data-binary @/tmp/big.bin "$B/files/upload?path=big.bin")" 507
check "keine Reste nach Quota-Fehler" "$(curl -s "${Bh[@]}" $B/me | jq .used_bytes)" 0

# Move / Delete / Changes
check "mkdir" "$(code "${A[@]}" -H 'content-type: application/json' -X POST $B/files/mkdir -d '{"path":"Archiv"}')" 201
check "move Ordner" "$(code "${A[@]}" -H 'content-type: application/json' -X POST $B/files/move -d '{"from":"Fotos","to":"Archiv/Fotos"}')" 200
check "Datei nach Move erreichbar" "$(curl -s "${A[@]}" "$B/files/download?path=Archiv/Fotos/2026/a.txt")" "neu"
check "alter Pfad weg" "$(code "${A[@]}" "$B/files/download?path=Fotos/2026/a.txt")" 404
C0=$(curl -s "${A[@]}" "$B/sync/changes" | jq .cursor)
check "Snapshot ohne Grabsteine" "$(curl -s "${A[@]}" "$B/sync/changes" | jq '[.entries[]|select(.deleted)]|length')" 0
check "delete Ordner" "$(curl -s "${A[@]}" -H 'content-type: application/json' -X POST $B/files/delete -d '{"path":"Archiv"}' | jq .deleted)" 4
check "Feed meldet Löschungen" "$(curl -s "${A[@]}" "$B/sync/changes?cursor=$C0" | jq '[.entries[]|select(.deleted)]|length')" 4
check "Speicher freigegeben" "$(curl -s "${A[@]}" $B/me | jq .used_bytes)" 0
check "Blob-Datei gelöscht" "$(find "$DATA/users/1/blobs" -type f | wc -l)" 0
check "Feed paginiert" "$(curl -s "${A[@]}" "$B/sync/changes?cursor=$C0&limit=2" | jq .has_more)" true

# Logout
check "logout" "$(code "${A[@]}" -X POST $B/auth/logout)" 200
check "Token danach ungültig" "$(code "${A[@]}" $B/me)" 401

echo; echo "$pass bestanden, $fail fehlgeschlagen"
[ $fail -eq 0 ]
