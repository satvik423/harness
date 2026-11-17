# ADR-023: Keyring access through `keyring-core`

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

The plan named the `keyring` crate. Its own documentation says applications should depend on `keyring-core` plus the specific credential store they need.

## Decision

Use `keyring-core` with `zbus-secret-service-keyring-store` (pure-Rust crypto, async-io runtime) on Linux. Other platforms fall back to environment variables until their stores are added.

## Consequences

Fewer dependencies and no tokio coupling in the keyring path; macOS and Windows stores come later.
