# ADR-019: OpenAI provider moved into Phase 1

> **Superseded in part by [ADR-026](0026-no-delegate-backends.md):** harness has no delegate backends.

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 1, approved by the user

## Context

The user has an OpenAI API key but no Anthropic API key. Anthropic's terms do
not allow a Claude Pro/Max subscription to be used by third-party tools
(ADR-008), so Phase 1's live exit check and recorded fixtures could not run
against Anthropic. The OpenAI provider (FR-PRV-02) was planned for Phase 12.

## Decision

Build `harness-provider-openai` (Responses API) in Phase 1. Add a top-level
`default_model` config key (default `anthropic/claude-opus-5-5`) and route
`harness run` to the provider named in the model. Move `ApiKey` and the backoff
calculation into `harness-ports` so both providers share them. The Anthropic
provider stays, verified against a mock server; its live fixtures are recorded
when an Anthropic API key is available.

## Consequences

Phase 1 can be used and verified end to end with OpenAI. Phase 12 shrinks to
fallback chains, the Anthropic OAuth profile and delegates. Phase 1 takes about a
day longer.
