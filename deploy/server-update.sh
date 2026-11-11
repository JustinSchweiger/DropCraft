#!/usr/bin/env bash
# Wird von update-server.ps1 auf dem Server ausgeführt.
# Aufruf: bash /opt/dropcraft-update.sh [neue-domain]
set -euo pipefail
NEW_DOMAIN="${1:-}"

command -v unzip >/dev/null || (apt-get update -q && apt-get install -y -q unzip)
cd /opt
unzip -o -q dropcraft.zip
cd dropcraft/deploy

if [ -n "$NEW_DOMAIN" ]; then
  OLD=$(grep -E '^DROPCRAFT_DOMAIN=' .env | cut -d= -f2- || true)
  if [ "$OLD" != "$NEW_DOMAIN" ]; then
    echo "==> Domain: $OLD -> $NEW_DOMAIN (alte Adresse funktioniert weiter)"
    sed -i "s|^DROPCRAFT_DOMAIN=.*|DROPCRAFT_DOMAIN=$NEW_DOMAIN|" .env
    ALIASES=$(grep -E '^DROPCRAFT_ALIASES=' .env | cut -d= -f2- || true)
    if [ -n "$OLD" ]; then
      case " $ALIASES " in
        *" $OLD "*) ;;
        *) ALIASES="$ALIASES $OLD" ;;
      esac
    fi
    ALIASES=$(echo $ALIASES)
    if grep -q '^DROPCRAFT_ALIASES=' .env; then
      sed -i "s|^DROPCRAFT_ALIASES=.*|DROPCRAFT_ALIASES=$ALIASES|" .env
    else
      echo "DROPCRAFT_ALIASES=$ALIASES" >> .env
    fi
  fi
fi

echo "==> Baue und starte den Server neu (dauert ein paar Minuten) ..."
docker compose up -d --build
docker compose restart caddy >/dev/null
sleep 5

DOMAIN=$(grep -E '^DROPCRAFT_DOMAIN=' .env | cut -d= -f2-)
echo
echo "Health-Check https://$DOMAIN/api/health :"
for i in 1 2 3 4 5 6; do
  if curl -fsS "https://$DOMAIN/api/health"; then
    echo
    exit 0
  fi
  echo "  (Zertifikat wird noch geholt, neuer Versuch in 5 s)"
  sleep 5
done
echo "Server antwortet noch nicht über HTTPS. Logs: docker compose logs caddy"
exit 1
