# ADR-022: `ToolRegistry` lives in ports

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

The plan put the registry in the `tools` crate, but `runtime` and `context` need it, and the layering rules forbid them to depend on an adapter.

## Decision

`ToolRegistry` (tools by name, specs sorted by name) is a small shared type in `harness-ports`.

## Consequences

Built-in, MCP and skill tools all register the same way; the layering rules stay intact.
