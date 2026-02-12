# Codex Implementation Instructions: `anki-backup-tool`

You are implementing a production-quality backup system for Anki data synced via AnkiWeb.

## 0) Goal

Build a Linux-first, headless daemon that performs hourly *change-aware* backups of Anki data and exposes a web UI + API for browsing backups, stats, rollback, and download.

It must not depend on the Anki desktop UI process running.

## 1) Hard requirements

1. Runs as a headless daemon on Linux.
2. Prefer Rust implementation.
3. Reuse/share code with upstream Anki where practical (especially sync/collection model logic).
4. Web interface listing historical backups.
5. Backup cadence defaults to hourly; skip if no data changes.
6. Roll back to any historical backup.
7. Download any backup.
8. Top-level backup list shows rough stats (total cards, decks, notes, revlog count).
9. Detail page shows deck names + cards-per-deck and per-backup metadata.
10. Include automated tests where possible.
11. High quality code and docs.

## 2) Non-goals (for v1)

- Windows/macOS service packaging (Linux only first).
- Multi-user tenancy.
- Browser extension/mobile app.

## 3) Architecture (proposed)

Implement a Rust workspace:

- `crates/core`
  - domain models (`Backup`, `BackupStats`, `DeckStats`, `BackupManifest`)
  - backup diff/hash utilities
  - rollback orchestration
- `crates/sync`
  - AnkiWeb sync adapter
  - strongly prefer reusing upstream Anki Rust crates/modules where licensing and interfaces allow
  - isolate any upstream-coupled code here
- `crates/storage`
  - local backup repository layout + metadata DB (SQLite)
  - integrity checks
- `crates/api`
  - HTTP API using `axum`
- `crates/web`
  - server-rendered UI (`askama`) or static frontend served by `axum`
- `crates/daemon`
  - scheduler, service lifecycle, config loading, structured logging

### Suggested runtime stack

- Rust stable
- Tokio async runtime
- Axum for HTTP
- SQLx + SQLite for metadata
- Serde + config + toml for config
- Tracing for logs
- Chrono for timestamps

## 4) Backup model

Each backup represents a consistent snapshot of local synchronized Anki collection state.

### Storage layout (example)

```
/var/lib/anki-backup-tool/
  backups/
    2026-02-13T01-00-00Z/
      collection.anki2.zst
      media-manifest.json
      metadata.json
      stats.json
  state/
    metadata.db
    current-pointer.json
    lock
```

### Change-aware strategy

At each hourly tick:
1. Sync from AnkiWeb.
2. Compute canonical content hash of collection + media manifest signature.
3. Compare with last successful backup hash.
4. If unchanged: record skipped run (`reason=unchanged`), do not create backup.
5. If changed: create new backup directory and metadata row.

## 5) Scheduler requirements

- Default schedule: once per hour on the hour.
- Configurable schedule and retention.
- Must prevent overlapping runs (global lock).
- Recovery: if daemon restarts, continue next scheduled run safely.

## 6) Web/API requirements

### UI pages

1. `GET /` backup list page
   - backup timestamp
   - size
   - cards/decks/notes/revlog rough counts
   - actions: View, Download, Rollback
2. `GET /backups/:id`
   - detailed stats
   - deck breakdown (name, card count)
   - metadata (hash, sync duration, source revision)
3. `POST /backups/:id/rollback`
   - rollback confirmation
4. `GET /backups/:id/download`
   - stream tar.zst or zip archive

### JSON API endpoints

- `GET /api/v1/backups`
- `GET /api/v1/backups/:id`
- `POST /api/v1/backups/:id/rollback`
- `GET /api/v1/backups/:id/download`
- `GET /api/v1/healthz`

## 7) Rollback semantics

Define rollback clearly:

- “Rollback” means restoring daemon-managed active state to selected snapshot.
- Perform atomic pointer swap + fsync.
- Record rollback event in metadata DB.
- Optional: trigger upload/sync back to AnkiWeb only behind explicit `--push-after-rollback` flag (default off for safety).

## 8) Config

Provide `/etc/anki-backup-tool/config.toml` plus env override.

Example:

```toml
[server]
listen = "127.0.0.1:8088"

[schedule]
cron = "0 * * * *"

[storage]
root = "/var/lib/anki-backup-tool"
retention_days = 90

[ankiweb]
username = "..."
password = "..."
# or token/session strategy if upstream supports securely

[security]
auth_enabled = true
```

Credentials must not be logged.

## 9) Security & reliability

- No secrets in logs.
- Use least-privilege filesystem permissions.
- Validate backup integrity after write.
- Expose read-only listing without auth only if explicitly configured.
- Rate-limit rollback endpoint.
- Add CSRF protection for UI POST endpoints.

## 10) Testing requirements

### Unit tests

- hash/diff logic
- stats extraction correctness
- retention pruning logic
- rollback pointer swap atomicity

### Integration tests

- API endpoints (list/detail/download/rollback)
- scheduler behavior (create vs skip unchanged)
- storage integrity and metadata consistency

### End-to-end smoke

- start daemon
- run initial backup
- run unchanged cycle (skip)
- mutate fixture -> backup created
- rollback to first backup

Target meaningful coverage on critical paths.

## 11) Deliverables

1. Working Rust workspace and buildable binaries.
2. Systemd unit file (`anki-backup-tool.service`).
3. Migrations for metadata SQLite DB.
4. API + UI implementation.
5. Tests and CI (GitHub Actions: fmt, clippy, test).
6. Documentation:
   - `README.md` quick start
   - `docs/ARCHITECTURE.md`
   - `docs/OPERATIONS.md`
   - `docs/ROLLBACK.md`

## 12) Milestones

M1: daemon + storage + basic backup creation
M2: change-aware hourly scheduling
M3: backup stats extraction + metadata
M4: web/API list + detail + download
M5: rollback + tests + hardening
M6: docs + CI polish

## 13) Quality bar

- Idiomatic Rust
- Robust error handling (`thiserror`/`anyhow`)
- Clear module boundaries
- No panic in normal runtime paths
- Deterministic tests

## 14) First implementation step

Start by scaffolding the Rust workspace and implementing `core + storage + daemon` enough to:

- run once,
- create a timestamped backup entry,
- compute/store content hash,
- skip when unchanged,
- expose `GET /api/v1/healthz`.

Then iterate milestone-by-milestone.
