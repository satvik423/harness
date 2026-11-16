# ADR-005: SQLite FTS5 (then sqlite-vec) for memory; RushDB and graphify optional

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Memory options considered: RushDB (graph + vector on Neo4j) and graphify (codebase knowledge graphs).

## Decision

Episodic and semantic memory use SQLite FTS5, later sqlite-vec, behind a `MemoryStore` trait. graphify can be added as a skill or MCP server for code exploration. RushDB can be added as a `MemoryStore` adapter if shared memory across agents or machines is needed.

## Consequences

Zero-ops local memory now, with a clear upgrade path.
