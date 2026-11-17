# ADR-015: Protocol events completed from the spec's flows

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 0 review, approved by the user

## Context

Tech spec 3.3 listed the `Event` variants, but flows F1–F3 and sections 3.13–3.14 rely on events missing from that list. Without them a transcript cannot be rebuilt from the event log (ADR-002).

## Decision

Add `UserInput`, `SystemNote`, `AssistantAborted`, `ApprovalResolved`, `CompactionStarted`, `SegmentOpened` and `UndoApplied`. The spec's "ApprovalGranted" is named `ApprovalResolved` because it also records denials.

## Consequences

The log is complete enough to replay any session exactly. Future additions follow the same test: every variant round-trips through JSON and `kind()` matches its tag.
