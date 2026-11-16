# ADR-006: Persona is a bundle; a subagent is a persona instance

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Personas and subagents overlap heavily.

## Decision

One `ResolvedPersona` configures every task, top-level or child.

## Consequences

One mechanism to build and test; any persona can be used as a subagent.
