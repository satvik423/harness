# ADR-012: Raw HTTP providers

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

There is no official Anthropic Rust SDK.

## Decision

`reqwest` + `eventsource-stream`, typed request and response structs, replay tests against recorded streams.

## Consequences

API changes are tracked by hand; fixture tests catch regressions.
