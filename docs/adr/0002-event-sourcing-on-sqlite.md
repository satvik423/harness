# ADR-002: Event sourcing on SQLite (WAL) as the single source of truth

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Pause, resume, crash recovery, recall and fork all need complete, durable history.

## Decision

Every meaningful action is a `harness_protocol::Event` stored in `.harness/state.db` and committed before any side effect that depends on it (NFR-03). JSONL is export-only.

## Consequences

One file to back up, transactional durability, full-text search via FTS5. Requires a dedicated writer thread (Phase 3).
