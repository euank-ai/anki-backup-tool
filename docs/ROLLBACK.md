# Rollback semantics

Rollback restores daemon-managed active state pointer to a selected historical backup.

Current implementation:
- validates backup exists and was actually created
- atomically updates `state/current-pointer.json`
- records rollback event in SQLite metadata

Safety:
- rollback endpoint has basic rate-limit (10 seconds)
- optional CSRF token hook via `ANKI_BACKUP_CSRF_TOKEN`

By default rollback does **not** push back to AnkiWeb.
