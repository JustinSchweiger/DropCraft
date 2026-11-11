﻿# DropCraft: Server-Update einspielen
# Lädt dropcraft.zip und das Update-Skript auf den VPS und führt es dort aus.
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
if ($Domain -and ($Domain -notmatch '^[A-Za-z0-9.-]+$')) {
    Write-Host "Ungültige Domain: $Domain" -ForegroundColor Red
    exit 1
}

# Das Server-Skript liegt in der Zip; für den Upload holen wir es heraus.
Add-Type -AssemblyName System.IO.Compression.FileSystem
$tmp = Join-Path $env:TEMP "dropcraft-update.sh"
$archive = [System.IO.Compression.ZipFile]::OpenRead($Zip)
try {
    $entry = $archive.Entries | Where-Object { $_.FullName -eq "dropcraft/deploy/server-update.sh" } | Select-Object -First 1
    if (-not $entry) {
        Write-Host "server-update.sh fehlt in der Zip – bitte die neue dropcraft.zip verwenden." -ForegroundColor Red
        exit 1
    }
    $reader = New-Object System.IO.StreamReader($entry.Open())
    $shText = $reader.ReadToEnd() -replace "`r", ""
    $reader.Close()
} finally {
    $archive.Dispose()
}
[System.IO.File]::WriteAllText($tmp, $shText, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "==> Lade Dateien auf $Server hoch ..." -ForegroundColor Cyan
scp "$Zip" "$tmp" "${User}@${Server}:/opt/"
if ($LASTEXITCODE -ne 0) {
    Write-Host "Hochladen fehlgeschlagen." -ForegroundColor Red
    exit 1
}

Write-Host "==> Führe das Update auf dem Server aus ..." -ForegroundColor Cyan
ssh "${User}@${Server}" "bash /opt/dropcraft-update.sh $Domain"
if ($LASTEXITCODE -ne 0) {
    Write-Host "Update auf dem Server fehlgeschlagen. Die Ausgabe oben zeigt den Grund." -ForegroundColor Red
    exit 1
}

Write-Host "==> Fertig, der Server läuft mit der neuen Version." -ForegroundColor Green
