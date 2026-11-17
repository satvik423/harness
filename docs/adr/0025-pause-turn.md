# ADR-025: `pause_turn` ends the task until server-side tools exist

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

Only server-side tools make a provider pause a turn. Continuing it means sending a request that ends with an assistant message, which the append-only transcript and the no-prefill rule do not allow, and harness uses no server-side tools yet.

## Decision

On `pause_turn` with no pending tool calls, the task ends with a `pause_turn` warning.

## Consequences

Revisit when server-side tools are added.
