# ADR-017: Phase 0 conventions

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 0 review, approved by the user

## Context

Several small choices were not fixed by the plan or differed from its snippets.

## Decision

(1) `ApprovalId` lives in `harness-types` with the other ids. (2) `Seq` is a newtype, not a bare `u64`, so event numbers cannot be confused with other counts. (3) Environment variables map to nested settings with double underscores: `HARNESS_POLICY__PROFILE=ci` sets `policy.profile`; `HARNESS_LOG` is reserved. (4) Workspace members are listed explicitly in the root `Cargo.toml` instead of a `crates/*` glob. (5) Ports are added in the phase that first uses them: Policy (Phase 2), HookRunner (Phase 7), MemoryStore (Phase 14); `Sandbox` and `ReadLedger` exist already because `ToolCtx` refers to them.

## Consequences

Each new crate needs one line in the workspace member list; port shapes are designed alongside their first real use.
