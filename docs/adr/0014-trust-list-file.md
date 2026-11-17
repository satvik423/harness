# ADR-014: The trusted-project list is a TOML file

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 0 review, approved by the user

## Context

SDD 2.9 placed the trusted-project list inside `user.db` (SQLite). Phase 0 needs the list for config loading, but SQLite only arrives in Phase 3, and the list will only ever hold a handful of paths.

## Decision

The list lives permanently in `~/.local/share/harness/trust.toml` (`version = 1`, `projects = [...]`). Paths are stored canonicalised; saves write a temporary file and rename it, so a crash never leaves a half-written file.

## Consequences

Human-readable and editable by hand; no database dependency for trust. `user.db` keeps its other roles (memory index) from Phase 14.
