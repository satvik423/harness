# ADR-040: Costs in the runtime, debug bodies rebuilt from the log, session changes from snapshots

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 4 (P4-T4, P4-T6, P4-T8); fills gaps in the spec

## Context

FR-PRV-06 asks for cost accounting, NFR-12 for `harness debug last-request` showing
"the exact body sent", and `/diff` for the session's changes. The spec does not say where
prices are applied, how bodies are captured, or what `/diff` compares.

## Decision

- **Costs:** `SessionConfig.price: Option<ModelPrice>` (from the config's pricing table,
  cache prices defaulting to the input price). The runtime fills `Usage.cost_micros`, so
  costs are stored with the events. Models without a price cost 0 and the TUI shows `$?`.
- **Debug bodies:** `harness_runtime::request_history` replays the log and returns the
  `ModelRequest` at each `TurnStarted`; the CLI turns it into the provider body with the
  adapter's own builder (deterministic), redacts key-shaped strings, and diffs the last
  two with an explicit append-only verdict. Nothing extra is stored.
- **Session changes:** `SnapshotStore::session_snapshots` returns each file's first
  snapshot in the session; `/diff` compares it with the file now.
- **The TUI** is a front-end crate depending only on types, protocol and ports; its
  `Backend` trait is implemented in `cli` over the runtime and the store.

## Consequences

Costs survive restarts and appear in exports. Debug output matches what the adapter sends
for the same request. `/diff` after an undo reflects the remaining changes.
