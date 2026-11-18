# ADR-034: Snapshot ports and the undo op

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 3 (P3-T7); details Tech spec 3.14 "Undo"

## Context

The spec says each tool snapshots its target files before writing, and `/undo`
restores the last turn's files, but defines no port, no op, and no meaning for "turn".

## Decision

- `ToolCtx` gains `snapshots: Arc<dyn FileSnapshots>`; `write_file`, `edit_file` and
  `apply_patch` call `before_write(path, previous contents)` right before writing
  (all files of a patch before any is written). If the snapshot cannot be stored,
  the file is not written.
- The runtime binds it to the session and the `AssistantMessage` whose calls are
  running: that message's sequence number is the "turn" (snapshots table `seq`).
  Previous contents go to the blob store; `NULL` means the file did not exist.
- `SnapshotStore::undo_last_turn` restores the latest turn's files (deleting files
  the turn created), then forgets that turn, so a second undo goes one turn further back.
- `Op::Undo { session }` runs it, records `UndoApplied`, and records a `SystemNote`
  that is sent at the start of the next user message
  (`SegmentContext::push_note`), so the model learns which files were reverted
  without any sent message changing.

## Consequences

Shell commands are not snapshotted (their writes cannot be known in advance); the
sandbox and version control cover them.
