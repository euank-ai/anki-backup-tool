# anki-backup-tool

Linux-first headless daemon for change-aware Anki backups with AnkiWeb sync integration, API, and web UI.

## Features

- Headless daemon (no desktop UI dependency)
- Real AnkiWeb sync integration path via configurable sync command
- Hourly change-aware backups (skip when unchanged)
- JSON API + simple web UI for list/detail/download/rollback
- Backup stats extraction from collection (`cards`, `decks`, `notes`, `revlog`)
- Atomic rollback pointer updates

## Quick start

```bash
cp config.example.toml /etc/anki-backup-tool/config.toml

# one-shot backup (sync + backup)
ANKIWEB_USERNAME=... \
ANKIWEB_PASSWORD=... \
ANKI_COLLECTION_PATH=/var/lib/anki/collection.anki2 \
ANKI_SYNC_COMMAND='python3 /opt/anki-sync/run_sync.py' \
/run/current-system/sw/bin/cargo run -p anki-backup-daemon -- run-once

# daemon mode (API/UI + hourly scheduler)
/run/current-system/sw/bin/cargo run -p anki-backup-daemon
```

## Environment

- `ANKI_BACKUP_ROOT` (default `./data`)
- `ANKI_BACKUP_LISTEN` (default `127.0.0.1:8088`)
- `ANKIWEB_USERNAME` / `ANKIWEB_PASSWORD`
- `ANKI_COLLECTION_PATH` (path to synchronized `collection.anki2`)
- `ANKI_SYNC_COMMAND` (real sync hook; invoked before backup)
- `ANKI_BACKUP_CSRF_TOKEN` (optional UI/API rollback guard token)

## Endpoints

- `GET /` list backups
- `GET /backups/:id` backup detail
- `POST /backups/:id/rollback`
- `GET /backups/:id/download` (tar stream)

JSON API:
- `GET /api/v1/healthz`
- `GET /api/v1/backups`
- `GET /api/v1/backups/:id`
- `POST /api/v1/backups/:id/rollback`
- `GET /api/v1/backups/:id/download`

See docs in `docs/` for architecture and operations.
