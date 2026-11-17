# ADR-008: Legitimate authentication only

> **Superseded in part by [ADR-026](0026-no-delegate-backends.md):** harness has no delegate backends.

- **Status:** Accepted
- **Date:** 2026-11-16
- **Source:** Building Harness, Part 5.2

## Context

Anthropic's terms forbid using Claude subscription logins in third-party tools.

## Decision

Auth is API keys, the Anthropic OAuth profile for API organisations, and official ChatGPT sign-in if the app can register. Delegate CLIs are off by default and gated on vendor terms. Config never contains secrets, only `keyring:` / `env:` references.

## Consequences

Subscriptions are usable only through delegates, where terms allow.
