# ADR-038: `/model` and `/effort` apply to a new session (until Phase 5)

- **Status:** Accepted; superseded in Phase 5 by ADR-046 (switching inside a session)
- **Date:** 2026-11-18
- **Source:** Decision by the user at the start of Phase 4 (P4-T4)

## Context

A segment's model and effort are frozen in its frame (ADR-032). Changing the model in
the middle of a segment invalidates earlier thinking blocks and the prompt cache
(guardrail 02). Compaction (Phase 5) is what lets a session move to a new segment.

## Decision

`/model` and `/effort` without an argument show the current and next values. With an
argument they set the value for the next session and ask "start a new session with … now?
[y/N]"; yes starts it (like `/clear`), and the current session stays saved and resumable.

## Consequences

Phase 5 can add switching inside a session by compacting into a new segment.
