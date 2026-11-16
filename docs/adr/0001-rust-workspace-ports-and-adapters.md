# ADR-001: Rust workspace with ports and adapters

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

The harness must let any component (model provider, storage, tools, sandbox) be replaced without breaking the others (G7, NFR-07).

## Decision

A `harness-ports` crate holds the traits. `harness-runtime` and `harness-context` depend only on `harness-ports`, `harness-protocol` and `harness-types` (plus runtime → context). Adapter crates implement the ports and are wired together only in `harness-cli`. The rule is enforced by `harness-testkit::arch` and the `workspace_respects_layering_rules` test.

## Consequences

Some boilerplate and `dyn` dispatch. In exchange every piece can be replaced and tested alone, and an accidental adapter import in the runtime fails CI.
