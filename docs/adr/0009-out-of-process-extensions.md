# ADR-009: Extensions run out of process

> **Superseded in part by [ADR-026](0026-no-delegate-backends.md):** harness has no delegate backends.

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Rust has no stable ABI; a crashing in-process plugin would take down the harness.

## Decision

Hooks are commands, MCP provides tools, delegates provide agents. Revisit WASM after v1.0.

## Consequences

A few milliseconds of process start-up; crashes and hangs stay isolated.
