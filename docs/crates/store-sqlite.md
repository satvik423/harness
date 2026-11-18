# harness-store-sqlite

SQLite storage for one project (P3-T1, P3-T2, Tech spec 3.12). An adapter crate;
the CLI wires it into the runtime.

## Layout (`<project>/.harness/`)

| Path | What |
|---|---|
| `state.db` (+ `-wal`, `-shm`) | sessions, tasks, events (+ FTS5), segments, snapshots, usage, task controls; anchors, pinned facts and memories are created for later phases |
| `blobs/ab/cdef…` | content-addressed bytes (BLAKE3), checked on every read |
| `locks/<session>.lock` | one process per session (`flock`) |
| `exports/` | `harness session export` output |
| `.gitignore` | written on first use: `state.db*`, `blobs/`, `exports/`, `locks/` |

## Public API

| Item | Purpose |
|---|---|
| `SqliteStore::open(project)` | Creates the layout, applies migrations (`PRAGMA user_version`), starts the writer thread. Blocking: call it before the async runtime or on a blocking thread |
| implements `EventLog`, `BlobStore`, `SessionStore`, `SnapshotStore` | the storage ports |
| `lock_session(id)`, `is_session_locked(id)`, `SessionLock`, `LockError` | the session lock (ADR-031) |
| `recent_inputs(limit)` | the user's latest messages across the project's sessions (the TUI's prompt history) |
| `SCHEMA_VERSION`, `STATE_DB`, `HARNESS_GITIGNORE` | constants |

## Design notes

- **Writes:** one writer thread owns the write connection. It takes every waiting
  job (up to 64), runs each in its own savepoint inside one transaction, commits,
  and only then answers each caller (NFR-03). A job that fails rolls back alone.
- **Reads:** a separate read-only connection, used on the blocking pool.
- **Settings:** WAL, `synchronous = NORMAL` (survives a killed process; a power loss can
  lose the last commits), foreign keys on, 5 s busy timeout (several processes may
  share the file).
- **Append rules:** the sequence number must be the session's last + 1 (for a fork,
  at least its fork point + 1); deltas are
  refused; unknown sessions are refused. Views are updated in the same transaction:
  `tasks` from `TaskState`, the session title from the first `UserInput`,
  `segments` and `current_segment` from `SegmentOpened`, `usage` from `Usage`,
  and (Phase 5, ADR-045) `anchors`, `pinned_facts` and the closed segment's row from
  `Compacted` (migration 0003 adds `pin_id` and `superseded_by`).
- **Forks (ADR-047):** a fork's events are read through its lineage: each ancestor's
  events up to the point its child forked, then its own. This holds for `range`,
  `last_seq` and `search`, through any depth. Its own numbering continues after the
  fork point. Nothing is copied.
- **Search:** FTS5 (`porter unicode61`) over prompts, replies, tool calls, tool output,
  notes and errors. Every query word is quoted, so FTS syntax cannot break a search.
- **Undo:** restores files first and forgets the turn afterwards, so an interrupted
  undo can be repeated.
- **Delete** (`SessionStore::delete_session`, ADR-041): takes the session lock, removes
  every row of the session in one transaction, then frees the blobs nothing references
  (one pass over all payloads and snapshots), only if no other session is open. Files
  are removed after their rows are committed.
- Migration `0002_task_controls.sql` adds the task control mailbox (ADR-031).
