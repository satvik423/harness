# ADR-021: `ToolCtx` carries `call_id`

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

Tools such as `shell` stream live output as `Event::ToolOutput { call_id, … }`, but the spec's `ToolCtx` did not say which call is running.

## Decision

Add `call_id: String` to `ToolCtx`.

## Consequences

One more field when the runtime builds a context; the UI can attach live output to the right call.
