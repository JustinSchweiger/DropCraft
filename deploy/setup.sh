#!/usr/bin/env bash
# Einrichtung von DropCraft auf einem frischen Debian/Ubuntu-VPS.
# Aufruf (als root, im Ordner deploy/):  ./setup.sh <domain>
# Ohne eigene Domain:                     ./setup.sh   (nimmt <ip>.sslip.io)
set -euo pipefail
cd "$(dirname "$0")"

if [ "$(id -u)" -ne 0 ]; then echo "Bitte als root ausführen."; exit 1; fi

IP=$(curl -4 -fsS https://api.ipify.org || hostname -I | awk '{print $1}')
DOMAIN=${1:-${IP//./-}.sslip.io}
echo "==> Domain: $DOMAIN"

echo "==> System aktualisieren"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q
apt-get upgrade -yq
apt-get install -yq ca-certificates curl ufw unattended-upgrades fail2ban

echo "==> Automatische Sicherheitsupdates"
dpkg-reconfigure -f noninteractive unattended-upgrades

echo "==> Firewall (SSH, HTTP, HTTPS)"
ufw allow OpenSSH
ufw allow 80/tcp
ufw allow 443
ufw --force enable

if ! command -v docker >/dev/null; then
  echo "==> Docker installieren"
  curl -fsSL https://get.docker.com | sh
fi

if [ ! -f .env ]; then
  cat > .env <<ENV
DROPCRAFT_DOMAIN=$DOMAIN
DROPCRAFT_DEFAULT_QUOTA_GB=20
DROPCRAFT_OPEN_REGISTRATION=false
RUST_LOG=dropcraft_server=info,tower_http=warn
ENV
fi
mkdir -p data && chown 10001:10001 data

echo "==> Bauen und starten (dauert beim ersten Mal einige Minuten)"
docker compose up -d --build

echo
echo "Fertig! Server: https://$DOMAIN"
echo "Test:  curl https://$DOMAIN/api/health"
echo "Admin-Account anlegen:"
echo "  docker compose exec -it dropcraft dropcraft-server adduser <name> <quota_gb> --admin"
