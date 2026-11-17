# ADR-020: `ProviderEvent::BlockComplete`

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

Tech spec 3.4 had the runtime rebuild the assistant message from streamed deltas plus `ToolCallEnd` and `Opaque` events. Rebuilding text from deltas can merge two separate text blocks (or drop an empty one), so the replayed message would differ from what the model produced. Claude binds its thinking blocks to the exact earlier conversation, so that difference breaks preserved thinking (ADR-003).

## Decision

Adapters emit `ProviderEvent::BlockComplete { block }` for each finished content block: text, a tool call with parsed input, or an opaque provider block. The runtime builds the assistant message only from these, in order. Deltas and `ToolCallStart`/`ToolCallInputDelta` are for live display. `ToolCallEnd` and `Opaque` are removed.

## Consequences

Transcripts match the model's output exactly on every provider. Adapters do the JSON parsing of tool inputs, where the wire format is known.
