# Operations

## Run one-shot backup

```bash
cargo run -p anki-backup-daemon -- --config config.toml run-once
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

- Secrets are never logged by daemon code paths.
