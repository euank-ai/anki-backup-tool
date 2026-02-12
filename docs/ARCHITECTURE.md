# Architecture

Workspace crates:
- `core`: domain types + hash utility
- `sync`: AnkiWeb sync adapter and real sync hook integration
- `storage`: backup repository, SQLite metadata, stats extraction, rollback pointer handling
- `daemon`: scheduler + API/UI server

Flow:
1. Scheduler tick (hourly, top-of-hour)
2. Sync adapter refreshes local collection from AnkiWeb (`ANKI_SYNC_COMMAND`)
3. Daemon hashes collection bytes
4. Storage compares hash with last created backup
5. Unchanged => insert skipped run; changed => persist new backup snapshot + metadata + stats

Rollback:
- Resolve target backup
- Atomically swap `state/current-pointer.json`
- Record rollback event in metadata DB
