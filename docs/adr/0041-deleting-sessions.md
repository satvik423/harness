# ADR-041: Deleting sessions, and empty sessions left by the TUI

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 4 trial follow-up; spec 3.16 lists `harness session delete ID`, but no
  phase task built it

## Context

The event log is append-only (ADR-002, ADR-003): nothing ever edits or removes an event
that a model has seen. Users still need a way to throw a whole session away. And every
time the TUI starts, it opens a new session, so opening harness and quitting without
typing left an empty session in the list.

## Decision

- **`SessionStore::delete_session(id)`** removes a session and everything stored for it
  (events, full-text rows, tasks, usage, segments, snapshots, control requests) in one
  transaction. It is a user action, the only way events leave the log. It refuses while
  the session is open in a harness process (it takes the session lock itself) and while
  other sessions are forked from it.
- **Blobs** are content-addressed and shared between sessions. After a delete, the store
  frees every blob that no event payload or snapshot references. Rule: every blob must be
  reachable from the log or a snapshot. Freeing runs only when no other session lock is
  held, because an open session may be writing a blob with the same bytes (its file is
  written before its row). When it is skipped, a later delete frees the leftovers.
- **`harness session delete ID`** asks for confirmation (`[y/N]`), or takes `--yes`;
  without a terminal and without `--yes` it refuses.
- **The TUI deletes a session it leaves** (quit, `/clear`, `/model`, `/sessions`) only if
  nobody typed into it (it has no title). It first waits for the session actor to stop
  (`SessionHandle::closed`), so nothing is written afterwards.
- **`harness -c` and `/sessions`** list only sessions with a prompt, most recently active
  first (the latest task change, else creation).

## Consequences

Deleting cannot be undone, hence the confirmation. Transcripts are never trimmed. A
session is either kept whole or removed whole. Empty sessions no longer pile up.
Freeing blobs reads every event payload once, which is fine for an occasional delete.
