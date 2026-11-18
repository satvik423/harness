# ADR-045: The validated compaction record travels in `Compacted`

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 5 (FR-CTX-03, Tech spec 3.12–3.13)

## Context

A compaction produces a summary, pinned facts and anchors. Several things need them
again:
- later compactions, to merge pins and continue anchor numbering;
- `recall`, to find anchors;
- replay after a restart;
- front-ends, to list anchors.

The event log is the source of truth (ADR-002), and the store already keeps tables
(`tasks`, `segments`) as projections of events.

## Decision

- **The record is part of the event.** `Event::Compacted` carries the validated record
  (`record: Box<CompactionRecord>`): goal, state of work, decisions, every pin so far
  (active or superseded, with the event that replaced it), files, questions, next
  steps, and the segment's anchors.
- **Shared types live in `protocol`.** The types (`CompactionRecord`, `PinnedFact`,
  `Anchor`, `KeyDecision`, `PinKind`) are in `harness-protocol`, so events can carry
  them; `harness-context` re-exports them.
- **The next segment is a normal segment.** It opens with an ordinary `SegmentOpened`,
  whose frozen frame holds the new opener (ADR-032). Replay needs nothing new to
  rebuild it.
- **The store keeps projections.** It fills `anchors` and `pinned_facts` from each
  record. Migration 0003 adds `pin_id` and `superseded_by` to `pinned_facts`. It also
  closes the compacted segment's row (`end_seq`, `tokens_before`, `tokens_after`).
- **Replay rebuilds the session's memory.** It reads pins and anchors from the
  `Compacted` events. Notes not yet sent move to the new segment, both live and on
  replay.

## Consequences

The log alone rebuilds everything, the tables are only for queries, and a record is
never edited. A record costs a few kilobytes per compaction, which is small.
