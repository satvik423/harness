# ADR-004: Anchored simple compaction instead of keep-tail or server-side compaction

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Plain summaries lose details; keep-tail compaction breaks preserved thinking; server compaction cannot carry harness anchors.

## Decision

Compaction produces a structured summary, verified verbatim pinned facts and descriptive anchors into the event log, plus a `recall` tool. Server compaction remains as an evaluation baseline.

## Consequences

One extra model call per compaction; details become restorable.
