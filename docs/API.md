# HTTP API

All endpoints are under `/api`. Authenticate with `Authorization: Bearer <token>`.

| Method | Path | Description |
|---|---|---|
| POST | `/auth/login` | `{username, password, device_name}` → `{token}` |
| POST | `/auth/logout` | Invalidate the token |
| POST | `/auth/password` | `{old_password, new_password}`, signs out other devices |
| GET | `/me` | Name, storage limit, storage used, current sync cursor |
| GET | `/files/list?path=Folder` | Contents of a folder |
| PUT | `/files/upload?path=a/b.txt&mtime=<ms>&base_rev=<rev>` | Upload a file (raw body). Parent folders are created. `base_rev` protects against conflicts, `0` = must not exist |
| GET | `/files/download?path=a/b.txt` | Download a file (headers `x-sha256`, `x-rev`) |
| POST | `/files/mkdir` | `{path}` |
| POST | `/files/delete` | `{path}`, folders recursively |
| POST | `/files/move` | `{from, to}`, also renames |
| GET | `/sync/changes?cursor=<n>&limit=1000` | All changes since `cursor`. `cursor=0` returns the full state |
| GET/POST | `/shares` | List links / create one `{path}` → `{token}`; public at `/s/{token}`. Creating returns the existing link for a path if there is one |
| DELETE | `/shares/{token}` | End a link |
| GET/POST | `/admin/users` | List / create accounts `{username, password, quota_gb}` |
| PATCH/DELETE | `/admin/users/{id}` | Change quota or password / delete the account |

## How a client syncs

1. On first start, `GET /sync/changes?cursor=0` (page with `has_more`), download everything and
   remember `cursor`.
2. Then regularly `GET /sync/changes?cursor=<remembered>` and apply only those changes.
3. Upload local changes with `PUT /files/upload?...&base_rev=<rev the client knows>`.
   On `409 Conflict` another device changed the file: keep the local version as a conflict copy.
