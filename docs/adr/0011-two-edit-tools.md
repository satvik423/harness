# ADR-011: Two edit tools

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Claude models do well with exact string replacement; OpenAI models are trained on a patch format.

## Decision

`edit_file` (exact replace + read-before-edit guard, backed by the `ReadLedger` port) and `apply_patch` (multi-file). Personas choose which to expose.

## Consequences

Slightly more code, better results on both providers.
