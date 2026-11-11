# DropCraft: Server-Update einspielen
# Lädt dropcraft.zip auf den VPS hoch, entpackt sie und baut den Server neu.
# Aufruf in PowerShell (im Ordner Dokumente\DropCraft):
#   .\update-server.ps1
#   .\update-server.ps1 -Domain dropcraft.tech   # Domain umstellen (alte bleibt als Zweitadresse)
# Daten, Accounts und die .env-Datei auf dem Server bleiben erhalten.

param(
    [string]$Server = "169.58.51.191",
    [string]$User = "root",
    [string]$Domain = "",
    [string]$Zip = (Join-Path $PSScriptRoot "dropcraft.zip")
)

$ErrorActionPreference = "Stop"

if (-not (Test-Path $Zip)) {
    Write-Host "dropcraft.zip nicht gefunden: $Zip" -ForegroundColor Red
    exit 1
}

Write-Host "==> Lade $(Split-Path $Zip -Leaf) auf $Server hoch ..." -ForegroundColor Cyan
scp "$Zip" "${User}@${Server}:/opt/dropcraft.zip"
if ($LASTEXITCODE -ne 0) {
    Write-Host "Hochladen fehlgeschlagen." -ForegroundColor Red
    exit 1
}

Write-Host "==> Entpacke und baue den Server neu (dauert ein paar Minuten) ..." -ForegroundColor Cyan
$remote = @'
set -e
command -v unzip >/dev/null || (apt-get update -q && apt-get install -y -q unzip)
cd /opt
unzip -o -q dropcraft.zip
cd dropcraft/deploy
NEW_DOMAIN="__DOMAIN__"
if [ -n "$NEW_DOMAIN" ]; then
  OLD=$(grep -E '^DROPCRAFT_DOMAIN=' .env | cut -d= -f2-)
  if [ "$OLD" != "$NEW_DOMAIN" ]; then
    echo "Domain: $OLD -> $NEW_DOMAIN (alte Adresse funktioniert weiter)"
    sed -i "s|^DROPCRAFT_DOMAIN=.*|DROPCRAFT_DOMAIN=$NEW_DOMAIN|" .env
    ALIASES=$(grep -E '^DROPCRAFT_ALIASES=' .env | cut -d= -f2- || true)
    case " $ALIASES " in *" $OLD "*) ;; *) ALIASES="$ALIASES $OLD" ;; esac
    ALIASES=$(echo $ALIASES)
    grep -q '^DROPCRAFT_ALIASES=' .env && sed -i "s|^DROPCRAFT_ALIASES=.*|DROPCRAFT_ALIASES=$ALIASES|" .env || echo "DROPCRAFT_ALIASES=$ALIASES" >> .env
  fi
fi
docker compose up -d --build
docker compose restart caddy >/dev/null
sleep 5
. ./.env
echo
echo "Health-Check:"
curl -fsS "https://$DROPCRAFT_DOMAIN/api/health" && echo
'@
# Windows-Zeilenenden entfernen, sonst stolpert bash darüber.
$remote = $remote -replace "`r", ""
$remote = $remote.Replace("__DOMAIN__", $Domain)
ssh "${User}@${Server}" $remote
if ($LASTEXITCODE -ne 0) {
    Write-Host "Update auf dem Server fehlgeschlagen. Die Ausgabe oben zeigt den Grund." -ForegroundColor Red
    exit 1
}

Write-Host "==> Fertig, der Server läuft mit der neuen Version." -ForegroundColor Green
