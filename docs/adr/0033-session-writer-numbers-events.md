# ADR-033: The session's writer numbers events; the journal replaces emit_with

- **Status:** Accepted (supersedes the mechanism of ADR-030)
- **Date:** 2026-11-18
- **Source:** Phase 3 (P3-T1, P3-T3)

## Context

ADR-030 let an event be built from its own sequence number through a synchronous
`EventSink::emit_with`. With the event log in place, a persisted event must be
committed (asynchronously) before it is broadcast and before anything depending on
it happens (NFR-03), so the sink can no longer be synchronous.

## Decision

- Each session has one writer, the runtime's `Journal`. It assigns sequence numbers,
  builds the event (optionally from its number), appends it to the `EventLog`, waits
  for the commit, then broadcasts it, all under one lock.
- `EventLog::append` takes a complete `LoggedEvent`; the store only checks that the
  number is exactly one more than the last (gapless, no duplicates), and rejects
  anything else with `StoreError::Conflict`. `AppendRequest` is removed and
  `last_seq` added.
- `EventSink::emit_with` is removed. Tools still use `EventSink::emit` for live,
  non-persisted output; live deltas carry the number of the last committed event.
- If an append fails, the task stops before the side effect that needed it, and a
  live `event_log` error tells front-ends.

## Consequences

Every broadcast persisted event is already durable. Numbering stays correct because a
session runs in one process at a time (ADR-031).
