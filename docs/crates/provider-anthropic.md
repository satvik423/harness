# harness-provider-anthropic

The Anthropic Messages API as a `Provider` (raw HTTP + SSE, ADR-012).

## Public API

| Item | Purpose |
|---|---|
| `AnthropicProvider::new(settings, key)` | The provider |
| `AnthropicSettings` | Base URL, API version (`2026-10-21`), betas, fallbacks, thinking-mismatch mode, retries, timeouts |
| `ApiKey` | The key; `Debug` prints `ApiKey(***)` |
| `build_messages_body`, `build_count_body` | Pure request mapping (snapshot-tested) |
| `StreamDecoder` | Pure SSE → `ProviderEvent` decoder |
| `parse_capabilities` | Models API object → `Capabilities` |
| `beta_header` | The `anthropic-beta` value for the settings |

## What goes on the wire

- `POST /v1/messages` with `stream: true`, `x-api-key`, `anthropic-version: 2026-10-21`.
- `thinking: {type: "adaptive", block_binding: {prefix_mismatch_behavior}}`
  (beta `thinking-binding-controls-2026-08-01`), plus `display: "summarized"` when
  the persona shows thinking.
- `output_config.effort` from the persona.
- Cache breakpoints: last system block, the opener block, and top-level automatic caching.
- `fallbacks: "default"` (beta `server-side-fallback-2026-07-01`) unless turned off.
- Never: `temperature`, `top_p`, `top_k`, assistant prefill, forced `tool_choice`.

## Reply handling

- Thinking and unknown block types become `ContentBlock::Opaque` and are replayed
  verbatim; opaque blocks from other providers are not sent.
- `input_transformations` entries become `Notice` events (they would signal a
  history edit, which ADR-003 forbids).
- Stop reasons map to `StopReason`, including `refusal` with its category.
- Empty text blocks are dropped (the API rejects them on input).

## Errors and retries

401/403 → `Auth`; 429 → `RateLimited`; 529 → `Overloaded`; other 5xx → `Server`;
other 4xx → `InvalidRequest`. Retryable errors are retried up to `max_attempts`
(default 4) before streaming starts, honouring `retry-after-ms` / `retry-after`
(capped at 60 s), otherwise backing off 1 s, 2 s, 4 s … (capped at 30 s) with up
to 25% jitter. Errors after streaming has started are not retried (that would
repeat output). Cancellation is checked while sending, while waiting to retry,
and between stream events.

## Tests

- Unit: body mapping (insta snapshot + targeted asserts), decoder, status mapping, backoff.
- `tests/http.rs`: wiremock server (headers, retries, retry-after, auth, cut streams, cancellation, count_tokens, capabilities).
- Live recordings are not made yet (no Anthropic API key); `fixtures/README.md` has the command.
- `examples/record_anthropic_fixtures.rs`: re-records those fixtures (needs an API key).
