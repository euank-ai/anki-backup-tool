# anki-backup-tool

Linux-first headless daemon for change-aware Anki backups with AnkiWeb sync integration, API, and web UI.

## Features

- **Headless daemon** — no desktop UI dependency
- **AnkiWeb sync** via direct protocol integration
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
cargo run -p anki-backup-daemon -- --config config.toml run-once

# Daemon mode (API/UI + hourly scheduler)
cargo run -p anki-backup-daemon -- --config config.toml
```

## Configuration

Configuration is loaded from a TOML file via the `--config` flag. Environment variables override config file values.

```bash
cargo run -p anki-backup-daemon -- --config config.toml
```

See `config.example.toml` for all available options.

### Environment variable overrides

| Variable | Config key | Default | Description |
|---|---|---|---|
| `ANKI_BACKUP_ROOT` | `storage.root` | `./data` | Root directory for backups and state DB |
| `ANKI_BACKUP_LISTEN` | `server.listen` | `127.0.0.1:8088` | Address for the HTTP server |
| `ANKIWEB_USERNAME` | `ankiweb.username` | — | AnkiWeb account email |
| `ANKIWEB_PASSWORD` | `ankiweb.password` | — | AnkiWeb account password |
| `ANKIWEB_ENDPOINT` | `ankiweb.endpoint` | — | Override AnkiWeb sync endpoint |
| `ANKI_BACKUP_RETENTION_DAYS` | `storage.retention_days` | `90` | Days to keep created backups before pruning |
| `ANKI_BACKUP_API_TOKEN` | `security.api_token` | — | Bearer token for API auth (optional) |
| `ANKI_BACKUP_CSRF_TOKEN` | `security.csrf_token` | — | CSRF token required for rollback (optional) |
| `DATABASE_URL` | `storage.database_url` | — | If starts with `postgres://`, uses Postgres; otherwise SQLite |

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
│   ├── sync/       # Direct AnkiWeb sync protocol client
│   └── daemon/     # Axum HTTP server, scheduler, Askama templates
├── docs/           # Architecture, operations, rollback docs
└── packaging/      # systemd service file
```

### Data flow

1. **Sync**: Collection is downloaded directly from AnkiWeb via sync protocol
2. **Hash**: SHA-256 of collection bytes is compared to last created backup
3. **Store**: If changed, collection is written to `backups/<timestamp>/collection.anki2`
4. **Stats**: Card/deck/note/revlog counts extracted from the SQLite collection
5. **Metadata**: Entry recorded in `state/metadata.db` (SQLite) or Postgres when `DATABASE_URL` is set
6. **Prune**: Backups older than retention period are deleted

### Database Backend

By default, metadata is stored in a local SQLite database at `$ANKI_BACKUP_ROOT/state/metadata.db`. For production or multi-instance deployments, you can use Postgres instead:

```bash
DATABASE_URL=postgres://user:pass@localhost:5432/anki_backup \
ANKI_BACKUP_ROOT=./data \
cargo run -p anki-backup-daemon
```

Tables are created automatically on first run. Backup files are always stored on the filesystem regardless of database backend.

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

# With Postgres backend
helm install anki-backup ./chart/anki-backup-tool \
  --set database.type=postgres \
  --set database.postgres.host=my-postgres \
  --set database.postgres.password=secret

# With Postgres using an existing secret (must contain DATABASE_URL key)
helm install anki-backup ./chart/anki-backup-tool \
  --set database.type=postgres \
  --set database.postgres.existingSecret=my-db-secret
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
