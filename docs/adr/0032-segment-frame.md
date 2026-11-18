# ADR-032: A segment's frozen prefix is stored as a frame

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 3 (P3-T3); needed to resume a segment exactly

## Context

A segment's system prompt, tools and model settings are frozen when it opens
(guardrails 02, 05, 06). Rebuilding them on resume from the current `AGENTS.md`,
tools and config could change the prefix, which breaks the prompt cache and
invalidates earlier thinking blocks. The spec stores the opener but not the prefix.

## Decision

`harness_context::SegmentFrame` holds everything frozen: provider and model, system
parts, tool specs, output limit, effort, thinking display and the opener. When a
segment opens, the frame is stored as a JSON blob and `Event::SegmentOpened` carries
its `frame: BlobRef`. Resuming rebuilds the segment with
`SegmentContext::from_frame` and replays its messages. The CLI builds the provider
from the frame, so a session keeps its model even if the default model changed.

## Consequences

`SegmentOpened` has a new field. New tool versions or instructions apply from the
next segment (Phase 5 compaction), not mid-segment.
