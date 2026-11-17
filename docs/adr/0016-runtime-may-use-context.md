# ADR-016: `runtime` may depend on `context`

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 0 review, approved by the user

## Context

SDD 2.4 lets `harness-runtime` depend on `harness-context`; CLAUDE.md rule 1 originally said runtime depends only on `ports`, `protocol` and `types`. The intent of both is that the core never depends on an adapter. The context engine is core logic in the same layer, not a swappable adapter, and the agent loop calls it every turn.

## Decision

`harness-runtime` may depend on `harness-context`. Neither may depend on any adapter. CLAUDE.md rule 1 is reworded to match; `harness_testkit::arch::harness_rules()` already encodes this.

## Consequences

No extra port or indirection for the context engine. The layering test still rejects any adapter import in either crate.
