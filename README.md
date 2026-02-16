# anki-backup-tool

Linux-first headless daemon for change-aware Anki backups with AnkiWeb sync integration, API, and web UI.

## Features

- **Headless daemon** — no desktop UI dependency
- **AnkiWeb sync** via configurable command hook
- **Change-aware** — skips backup when collection is unchanged
- **Compressed downloads** — tar + zstd (`.tar.zst`)
- **JSON API** + templated web UI (Askama) for list/detail/download/rollback
- **Backup stats** extracted from collection (cards, decks, notes, revlog)
- **Retention pruning** — automatic cleanup of old backups
- **Atomic rollback** pointer updates
- **API auth** via Bearer token; CSRF protection on rollback

## Quick Start

```bash
# One-shot backup (sync + backup)
ANKIWEB_USERNAME=you@example.com \
ANKIWEB_PASSWORD=secret \
ANKI_COLLECTION_PATH=/path/to/collection.anki2 \
ANKI_SYNC_COMMAND='python3 /opt/anki-sync/run_sync.py' \
cargo run -p anki-backup-daemon -- run-once

# Daemon mode (API/UI + hourly scheduler)
ANKI_BACKUP_ROOT=./data \
ANKI_BACKUP_LISTEN=127.0.0.1:8088 \
cargo run -p anki-backup-daemon
```

## Configuration

All configuration is via environment variables:

| Variable | Default | Description |
|---|---|---|
| `ANKI_BACKUP_ROOT` | `./data` | Root directory for backups and state DB |
| `ANKI_BACKUP_LISTEN` | `127.0.0.1:8088` | Address for the HTTP server |
| `ANKIWEB_USERNAME` | — | AnkiWeb account email |
| `ANKIWEB_PASSWORD` | — | AnkiWeb account password |
| `ANKI_COLLECTION_PATH` | — | Path to synchronized `collection.anki2` |
| `ANKI_SYNC_COMMAND` | — | Shell command to sync before reading collection |
| `ANKI_BACKUP_RETENTION_DAYS` | `90` | Days to keep created backups before pruning |
| `ANKI_BACKUP_API_TOKEN` | — | Bearer token for API auth (optional) |
| `ANKI_BACKUP_CSRF_TOKEN` | — | CSRF token required for rollback (optional) |

## API Reference

### Web UI

| Method | Path | Description |
|---|---|---|
| `GET` | `/` | Backup list page (HTML) |
| `GET` | `/backups/{id}` | Backup detail page (HTML) |
| `GET` | `/backups/{id}/download` | Download backup as `.tar.zst` |
| `POST` | `/backups/{id}/rollback` | Rollback to this backup |

### JSON API

All API endpoints require `Authorization: Bearer <token>` when `ANKI_BACKUP_API_TOKEN` is set.

| Method | Path | Description |
|---|---|---|
| `GET` | `/api/v1/healthz` | Health check (`{"status":"ok"}`) |
| `GET` | `/api/v1/backups` | List all backups (JSON array) |
| `GET` | `/api/v1/backups/{id}` | Backup detail (JSON) |
| `GET` | `/api/v1/backups/{id}/download` | Download backup as `.tar.zst` |
| `POST` | `/api/v1/backups/{id}/rollback` | Rollback (requires `x-csrf-token` header if configured) |

## Architecture

```
anki-backup-tool/
├── crates/
│   ├── core/       # Domain types: BackupEntry, BackupStats, content hashing
│   ├── storage/    # SQLite metadata DB + file-based backup repository
│   ├── sync/       # AnkiWeb sync via command hook
│   └── daemon/     # Axum HTTP server, scheduler, Askama templates
├── docs/           # Architecture, operations, rollback docs
└── packaging/      # systemd service file
```

### Data flow

1. **Sync**: `sync_command` is executed with AnkiWeb credentials in env
2. **Hash**: SHA-256 of collection bytes is compared to last created backup
3. **Store**: If changed, collection is written to `backups/<timestamp>/collection.anki2`
4. **Stats**: Card/deck/note/revlog counts extracted from the SQLite collection
5. **Metadata**: Entry recorded in `state/metadata.db` (SQLite)
6. **Prune**: Backups older than retention period are deleted

### Scheduler

In daemon mode, the scheduler runs every hour on the hour. Each cycle:
- Syncs collection from AnkiWeb
- Creates backup if content changed (skips if unchanged)
- Prunes old backups past retention period

## Docker

```bash
# Pull the image
docker pull ghcr.io/euank-ai/anki-backup-tool:latest

# Run the daemon
docker run -d \
  -p 8088:8088 \
  -v anki-data:/data \
  -e ANKIWEB_USERNAME=you@example.com \
  -e ANKIWEB_PASSWORD=secret \
  -e ANKI_COLLECTION_PATH=/data/collection.anki2 \
  ghcr.io/euank-ai/anki-backup-tool:latest
```

## Helm

```bash
# Add and install
helm install anki-backup ./chart/anki-backup-tool \
  --set env.ANKIWEB_USERNAME=you@example.com \
  --set env.ANKIWEB_PASSWORD=secret

# Or use an existing secret for credentials
helm install anki-backup ./chart/anki-backup-tool \
  --set existingSecret=my-anki-secret

# With persistence and ingress
helm install anki-backup ./chart/anki-backup-tool \
  --set persistence.size=5Gi \
  --set ingress.enabled=true \
  --set ingress.hosts[0].host=anki.example.com \
  --set ingress.hosts[0].paths[0].path=/ \
  --set ingress.hosts[0].paths[0].pathType=Prefix
```

See `chart/anki-backup-tool/values.yaml` for all configurable values.

## Development

```bash
# Run all tests
cargo test --workspace

# Run integration tests only
cargo test -p anki-backup-daemon --test integration
```

## License

MIT
