# Operations

## Run one-shot backup

Set required sync env vars and run:

```bash
ANKIWEB_USERNAME=... ANKIWEB_PASSWORD=... \
ANKI_COLLECTION_PATH=/path/to/collection.anki2 \
ANKI_SYNC_COMMAND='python3 /opt/anki-sync/run_sync.py' \
/run/current-system/sw/bin/cargo run -p anki-backup-daemon -- run-once
```

## Run daemon

```bash
/run/current-system/sw/bin/cargo run -p anki-backup-daemon
```

## Data layout

```
$ANKI_BACKUP_ROOT/
  backups/<timestamp>/collection.anki2
  backups/<timestamp>/metadata.json
  state/metadata.db
  state/current-pointer.json
```

## Health check

`GET /api/v1/healthz`

## Notes

- Ensure `ANKI_SYNC_COMMAND` performs actual AnkiWeb sync.
- Secrets are never logged by daemon code paths.
