# ADR-030: `EventSink::emit_with` for events that name their own sequence number

- **Status:** Superseded by ADR-033 (the journal assigns numbers and commits before broadcasting)
- **Date:** 2026-11-17
- **Source:** Phase 2 (P2-T3); needed by Tech spec 3.5 output capping

## Context

The capped tool result ends with `[output trimmed: … · full: recall {"seq_from":S,"seq_to":S}]`,
where `S` is the sequence number of the `ToolFinished` event that holds the full output.
That number is assigned when the event is emitted, but the capped text is part of the
same event.

## Decision

`EventSink` gains `emit_with(&self, build: &mut dyn FnMut(Seq) -> Event) -> Seq`. The sink
assigns the number and calls `build` with it while holding its lock, so no other event
can take the number. `BroadcastSink` now uses a mutex instead of an atomic counter, which
also guarantees subscribers see events in sequence order when parallel tools emit.

## Consequences

Every `EventSink` implements `emit_with`. From Phase 3 the event log assigns sequence
numbers to persisted events, and must keep the same guarantee.
