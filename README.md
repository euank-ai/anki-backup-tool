# anki-backup-tool

Headless Anki backup daemon (Rust workspace), implemented milestone-by-milestone.

## Current status (M1)

Implemented:
- Rust workspace scaffold (`core`, `storage`, `daemon`)
- One-shot backup flow (`run-once`)
- Timestamped backup directory creation
- Content hash compute/store
- Unchanged backup skip behavior
- `GET /api/v1/healthz` endpoint
- Unit tests for hash + skip behavior

## Run

```bash
# one-shot backup run
cargo run -p anki-backup-daemon -- run-once

# start API server (health check)
cargo run -p anki-backup-daemon
```

Environment variables:
- `ANKI_BACKUP_ROOT` (default `./data`)
- `ANKI_BACKUP_LISTEN` (default `127.0.0.1:8088`)
- `ANKI_BACKUP_COLLECTION_SOURCE` (optional content source for M1 run-once)
