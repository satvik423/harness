# ADR-013: Crate package names use the `harness-` prefix

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 0 review, approved by the user

## Context

The plan used short package names (`types`, `config`, `ports`). Short names are generic: `types` and `config` already exist as unrelated crates on crates.io, which makes `use` paths, docs and error messages ambiguous and risks name clashes if such a crate is ever pulled in.

## Decision

Every workspace package is named `harness-<name>` (library path `harness_<name>`). Folder names stay as in the plan (`crates/types`).

## Consequences

Slightly longer names; unambiguous paths everywhere. New crates follow the same pattern (for example `harness-provider-anthropic`).
