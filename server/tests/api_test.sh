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

# Range-Downloads
printf '0123456789' > /tmp/r.txt
curl -s -o /dev/null "${A[@]}" -X PUT --data-binary @/tmp/r.txt "$B/files/upload?path=r.txt"
check "Range 2-5" "$(curl -s "${A[@]}" -H 'Range: bytes=2-5' "$B/files/download?path=r.txt")" "2345"
check "Range Status 206" "$(code "${A[@]}" -H 'Range: bytes=2-' "$B/files/download?path=r.txt")" 206
check "Range Suffix" "$(curl -s "${A[@]}" -H 'Range: bytes=-3' "$B/files/download?path=r.txt")" "789"

# Freigaben
mkdir -p /tmp/sh && echo "Hallo Welt" > "/tmp/sh/Grüße & <Test>.txt"
curl -s -o /dev/null "${A[@]}" -X PUT --data-binary "@/tmp/sh/Grüße & <Test>.txt" "$B/files/upload?path=Teilen/Grüße%20%26%20%3CTest%3E.txt"
curl -s -o /dev/null "${A[@]}" -X PUT --data-binary @/tmp/r.txt "$B/files/upload?path=Teilen/Unter/zahlen.txt"
curl -s -o /dev/null "${A[@]}" -X PUT --data-binary @/tmp/r.txt "$B/files/upload?path=Privat/geheim.txt"
TOK=$(J "${A[@]}" -X POST $B/shares -d '{"path":"Teilen"}' | jq -r .token)
check "Freigabe angelegt" "${#TOK}" 24
check "gleicher Pfad -> gleicher Link" "$(J "${A[@]}" -X POST $B/shares -d '{"path":"Teilen"}' | jq -r .token)" "$TOK"
S=http://127.0.0.1:18080/s/$TOK
check "Seite erreichbar ohne Login" "$(code $S)" 200
check "Dateiname escaped" "$(curl -s $S | grep -c 'Grüße &amp; &lt;Test&gt;.txt')" 1
check "kein rohes <Test> im HTML" "$(curl -s $S | grep -c '<Test>')" 0
check "Unterordner" "$(curl -s "$S?p=Unter" | grep -c 'zahlen.txt')" 1
check "Datei laden" "$(curl -s "$S/file?p=Unter/zahlen.txt")" "0123456789"
check "Ausbruch mit .. verhindert" "$(code "$S/file?p=../Privat/geheim.txt")" 404
check "Ausbruch kodiert verhindert" "$(code "$S/file?p=..%2FPrivat%2Fgeheim.txt")" 404
check "falscher Token" "$(code http://127.0.0.1:18080/s/abc)" 404
curl -s -o /tmp/t.zip "$S/zip"
check "ZIP gültig" "$(python3 -c "import zipfile;z=zipfile.ZipFile('/tmp/t.zip');print(sorted(z.namelist()), z.read('Unter/zahlen.txt').decode())")" "['Grüße & <Test>.txt', 'Unter/zahlen.txt'] 0123456789"
# Gast-Upload, Einstellungen, Passwort, Ablauf, Vorschau
check "Upload standardmäßig aus" "$(code -F "files=@/tmp/r.txt" "$S/upload")" 403
check "Einstellungen ändern" "$(J "${A[@]}" -X PATCH $B/shares/$TOK -d '{"allow_upload":true,"notify":true}' | jq -c '[.allow_upload,.notify,.has_password]')" '[true,true,false]'
check "Seite zeigt Upload-Formular" "$(curl -s $S | grep -c 'enctype="multipart/form-data"')" 1
echo "vom gast" > /tmp/gast.txt
check "Gast-Upload leitet zurück" "$(code -F "name=Anna" -F "files=@/tmp/gast.txt;filename=Grüße & <Test>.txt" "$S/upload")" 303
check "Name-Konflikt -> neue Datei" "$(curl -s "${A[@]}" "$B/files/download?path=Teilen/Grüße%20%26%20%3CTest%3E%20(2).txt")" "vom gast"
check "Original unverändert" "$(curl -s "${A[@]}" "$B/files/download?path=Teilen/Grüße%20%26%20%3CTest%3E.txt")" "Hallo Welt"
check "Urheber im Feed" "$(curl -s "${A[@]}" "$B/sync/changes?cursor=0" | jq -r '.entries[] | select(.path=="Teilen/Grüße & <Test> (2).txt") | .modified_by')" "Gast: Anna"
check "Urheber der App" "$(curl -s "${A[@]}" "$B/sync/changes?cursor=0" | jq -r '.entries[] | select(.path=="r.txt") | .modified_by')" "test"
check "Gerät statt Name" "$(code -A 'Mozilla/5.0 (iPhone; CPU iPhone OS 18_0 like Mac OS X) Version/18.0 Mobile/15E148 Safari/604.1' -F "files=@/tmp/gast.txt;filename=handy.txt" "$S/upload")" 303
check "Urheber = Gerät" "$(curl -s "${A[@]}" "$B/sync/changes?cursor=0" | jq -r '.entries[] | select(.path=="Teilen/handy.txt") | .modified_by')" "Gast · iPhone · Safari"
check "Upload-Skript" "$(curl -s -o /dev/null -w '%{content_type}' "http://127.0.0.1:18080/assets/upload.js")" "text/javascript; charset=utf-8"
check "Upload in Unterordner" "$(code -F "files=@/tmp/gast.txt;filename=neu.txt" "$S/upload?p=Unter")" 303
check "landet im Unterordner" "$(curl -s "${A[@]}" "$B/files/download?path=Teilen/Unter/neu.txt")" "vom gast"
check "Pfad im Dateinamen entschärft" "$(code -F "files=@/tmp/gast.txt;filename=../../Privat/x.txt" "$S/upload")" 303
check "kein Ausbruch per Dateiname" "$(code "${A[@]}" "$B/files/download?path=Privat/x.txt")" 404
check "Vorschau-Seite" "$(code "$S/view?p=Unter/zahlen.txt")" 200
check "Passwort setzen" "$(J "${A[@]}" -X PATCH $B/shares/$TOK -d '{"password":"geheim42"}' | jq .has_password)" true
check "ohne Passwort gesperrt" "$(code $S)" 401
check "Datei ohne Passwort gesperrt" "$(code "$S/file?p=Unter/zahlen.txt")" 401
check "falsches Passwort" "$(code -d password=falsch "$S/auth")" 401
COOKIE=$(curl -s -o /dev/null -D - -d password=geheim42 "$S/auth" | grep -i '^set-cookie' | sed 's/^[^:]*: //; s/;.*//' | tr -d '\r')
check "richtiges Passwort -> Cookie" "${COOKIE:0:4}" "dcs_"
check "mit Cookie offen" "$(code -H "Cookie: $COOKIE" $S)" 200
check "Passwort entfernen" "$(J "${A[@]}" -X PATCH $B/shares/$TOK -d '{"password":""}' | jq .has_password)" false
check "wieder offen" "$(code $S)" 200
check "Ablauf in Vergangenheit" "$(J "${A[@]}" -X PATCH $B/shares/$TOK -d '{"expires_at":1000}' | jq .expires_at)" 1000
check "abgelaufen" "$(code $S)" 410
check "Ablauf entfernen" "$(J "${A[@]}" -X PATCH $B/shares/$TOK -d '{"expires_at":0}' | jq .expires_at)" null
check "Startseite" "$(curl -s http://127.0.0.1:18080/ | grep -c 'Private Cloud')" 1
# Updates
mkdir -p "$DATA/public" && echo '{"version":"0.1.5"}' > "$DATA/public/latest.json"
check "Update-Info" "$(curl -s http://127.0.0.1:18080/updates/latest.json | jq -r .version)" "0.1.5"
check "Update unbekannte Datei" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/updates/geheim.db)" 404
check "Update fehlende Datei" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/updates/DropCraft-macOS.zip)" 404
# Web-App und signierte Links
check "Web-App" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/app/)" 200
check "Web-App-Skript" "$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18080/app/app.js)" 200
check "Web-App ohne Pfadausbruch" "$(curl -s -o /dev/null -w '%{http_code}' 'http://127.0.0.1:18080/app/..%2FCargo.toml')" 404
echo "signiert" > /tmp/sig.txt
curl -s -o /dev/null "${A[@]}" -X PUT --data-binary @/tmp/sig.txt "$B/files/upload?path=sig.txt"
DL=$(curl -s "${A[@]}" "$B/files/link?path=sig.txt" | jq -r .url)
check "Signierter Link" "$(curl -s "http://127.0.0.1:18080$DL")" "signiert"
check "Link ohne Anmeldung nicht erstellbar" "$(curl -s -o /dev/null -w '%{http_code}' "$B/files/link?path=sig.txt")" 401
check "Gefälschte Signatur" "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:18080${DL/s=/s=0}")" 403
check "Anderer Pfad mit Signatur" "$(curl -s -o /dev/null -w '%{http_code}' "http://127.0.0.1:18080${DL/sig.txt/x.txt}")" 403
check "kein App-Download ohne Datei" "$(code http://127.0.0.1:18080/download/windows)" 404

check "Anna kann Justins Freigabe nicht löschen" "$(code "${Bh[@]}" -X DELETE $B/shares/$TOK)" 404
check "Liste" "$(curl -s "${A[@]}" $B/shares | jq length)" 1
check "Freigabe löschen" "$(code "${A[@]}" -X DELETE $B/shares/$TOK)" 200
check "danach weg" "$(code $S)" 404

# Logout
check "logout" "$(code "${A[@]}" -X POST $B/auth/logout)" 200
check "Token danach ungültig" "$(code "${A[@]}" $B/me)" 401

echo; echo "$pass bestanden, $fail fehlgeschlagen"
[ $fail -eq 0 ]
