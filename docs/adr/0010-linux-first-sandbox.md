# ADR-010: Linux-first sandbox via bubblewrap

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

The main development platform is Linux.

## Decision

bubblewrap with network off by default, Landlock as fallback, macOS seatbelt as P1, Windows unsandboxed with warnings. The `Sandbox` port exists from Phase 0.

## Consequences

Strong safety on the primary platform; portable later.
