# ADR-003: Append-only transcripts with segments

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Current Claude models bind thinking blocks to the exact prior conversation, and every provider caches by prompt prefix.

## Decision

Sent history is never edited, reordered or trimmed within a segment. Tool output is capped when first recorded. Compaction opens a new segment.

## Consequences

Needs careful request layout and write-time capping. Gives maximum cache hits and valid preserved thinking.
