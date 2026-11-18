# ADR-035: Fork moves to Phase 5

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Decision by the user at the start of Phase 3

## Context

Fork (FR-SES-06, P2) was planned for Phase 3 "if time allows". The spec says a fork
in the middle of a segment starts with a compaction of the parent's segment, because
the parent's thinking blocks belong to its conversation; compaction arrives in Phase 5.

## Decision

Fork is built in Phase 5, together with compaction, so it works at any event. Phase 3
keeps the schema columns (`parent_id`, `forked_at_seq`) ready, and `Op::Fork` answers
"fork arrives with compaction in Phase 5".

## Consequences

FR-SES-06 moves from Phase 3 to Phase 5.
