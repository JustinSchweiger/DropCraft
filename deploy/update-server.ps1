# DropCraft: Server-Update einspielen
# Lädt dropcraft.zip auf den VPS hoch, entpackt sie und baut den Server neu.
# Aufruf in PowerShell (im Ordner Dokumente\DropCraft):
#   .\update-server.ps1
# Daten, Accounts und die .env-Datei auf dem Server bleiben erhalten.

param(
    [string]$Server = "169.58.51.191",
    [string]$User = "root",
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
docker compose up -d --build
sleep 3
. ./.env
echo
echo "Health-Check:"
curl -fsS "https://$DROPCRAFT_DOMAIN/api/health" && echo
'@
# Windows-Zeilenenden entfernen, sonst stolpert bash darüber.
$remote = $remote -replace "`r", ""
ssh "${User}@${Server}" $remote
if ($LASTEXITCODE -ne 0) {
    Write-Host "Update auf dem Server fehlgeschlagen. Die Ausgabe oben zeigt den Grund." -ForegroundColor Red
    exit 1
}

Write-Host "==> Fertig, der Server läuft mit der neuen Version." -ForegroundColor Green
