# Running the server

The server is a single Rust binary (axum + SQLite). Every account has its own folder
`data/users/<id>/blobs/` and its own storage limit. The database stores path, size, SHA-256 and a
revision number per file. Deleted files stay as tombstones in the database so other devices learn
about the deletion when they sync.

## Install on a VPS

```sh
# as root on the server
apt-get update && apt-get install -y git
git clone https://github.com/JustinSchweiger/DropCraft.git /opt/dropcraft
cd /opt/dropcraft/deploy
./setup.sh                    # without a domain: https://<ip-with-dashes>.sslip.io
./setup.sh cloud.example.com  # with your own domain (A record pointing to the server)

# first admin (asks for the password)
docker compose exec -it dropcraft dropcraft-server adduser justin 100 --admin
```

| Task | Command |
|---|---|
| Update | `git pull && docker compose up -d --build` |
| Logs | `docker compose logs -f dropcraft` |
| Backup | Back up `deploy/data/` (database and all files) |

**A second server** (for example only for one other account): install it the same way and
create just that one account. Without `--admin` it is a normal user; change its storage with
`docker compose exec dropcraft dropcraft-server …` or create the account again.

## Settings (`deploy/.env`)

| Variable | Meaning | Default |
|---|---|---|
| `DROPCRAFT_DOMAIN` | Domain for HTTPS | – |
| `DROPCRAFT_DEFAULT_QUOTA_GB` | Storage limit for new accounts | 20 |
| `DROPCRAFT_OPEN_REGISTRATION` | Anyone may register | false |
| `DROPCRAFT_ALIASES` | More addresses, e.g. an old domain | – |

## Automatic deployment (GitHub Actions)

- **CI** (`.github/workflows/ci.yml`) runs on every push: server tests, sync engine tests, Windows
  builds with and without `cloud-files`, macOS and iOS builds. A green run on `main` creates the
  release `v0.1.<run number>`.
- **Deploy** (`.github/workflows/deploy.yml`) runs after a green CI on `main`: it updates the server
  and publishes the apps with `latest.json` under `https://<domain>/updates/`. The Windows app is
  also offered at `https://<domain>/download/windows` (start page).

One-time setup on the server:

```sh
ssh-keygen -t ed25519 -N "" -C github-deploy -f ~/.ssh/github_deploy
cat ~/.ssh/github_deploy.pub >> ~/.ssh/authorized_keys
cat ~/.ssh/github_deploy        # copy the private key
```

Then add two secrets under **Settings › Secrets and variables › Actions**: `DEPLOY_HOST`
(e.g. `dropcraft.tech`) and `DEPLOY_SSH_KEY` (the whole private key, including the BEGIN and END lines).

## n8n on the same server

`deploy/docker-compose.yml` also starts n8n at `https://n8n.dropcraft.tech` (another name:
`N8N_DOMAIN=…` in `deploy/.env`). It needs a DNS A record `n8n` pointing to the server.

`N8N_ENCRYPTION_KEY` in `deploy/.env` is created on the first update and must never change
afterwards, or stored credentials become unreadable. Update n8n with
`cd /opt/dropcraft/deploy && docker compose pull n8n && docker compose up -d n8n`.

## Musik-Check

`https://<domain>/musik/` is a small page for checking spliced music files (for example for
carnival shows): structure with cuts and tempo, loudness per part (LUFS, EBU R128), clipping,
tips, and an evened-out WAV to download. The analysis runs entirely in the browser; the music is
not uploaded. Files: `deploy/musik/`, served by Caddy.
