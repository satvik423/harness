# ADR-026: No delegate backends

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Decision by the user, 17 Nov 2026

## Context

The plan (FR-PRV-08, Phase 12) included delegate backends: running the official `claude` or `codex` CLI as an external agent, so a subscription could be used. Inside such a task the vendor's agent loop, tools and permissions run instead of harness's, which defeats the purpose of building harness.

## Decision

harness never hands tasks to external agent CLIs. FR-PRV-08 and the `provider-delegate` crate are removed from the plan, and the `[providers.delegate]` config section is removed from the code. Models are reached only through provider APIs with API keys (or an official sign-in program for a provider API, if one becomes available).

## Consequences

A Claude subscription cannot be used with harness; Claude needs an Anthropic API key. This supersedes the delegate parts of ADR-008, ADR-009 and ADR-019.
