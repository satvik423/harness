# harness-provider-openai

The OpenAI Responses API as a `Provider` (raw HTTP + SSE). Added in Phase 1 by
ADR-019 (originally planned for Phase 12).

## Public API

| Item | Purpose |
|---|---|
| `OpenAiProvider::new(settings, key)` | The provider |
| `OpenAiSettings` | Base URL, `store` (default `false`), retries, timeouts (read timeout 600 s, since reasoning can take a while before the first event) |
| `ApiKey` | Re-exported from `harness-ports`; `Debug` prints `ApiKey(***)` |
| `build_responses_body` | Pure request mapping (snapshot-tested) |
| `ResponsesDecoder` | Pure SSE → `ProviderEvent` decoder |
| `default_capabilities` | Conservative capabilities (the Models API reports no limits) |

## Mapping

| Harness | Responses API |
|---|---|
| System parts | `instructions` (joined with blank lines) |
| User text | `{"type":"message","role":"user","content":[{"type":"input_text"}]}` |
| Assistant text | `{"type":"message","role":"assistant","content":[{"type":"output_text"}]}` |
| Tool call | `{"type":"function_call","call_id","name","arguments"}` |
| Tool result | `{"type":"function_call_output","call_id","output"}` (`"Error: …"` when it failed) |
| System note | `{"type":"message","role":"developer"}` |
| Opaque `openai` block | replayed verbatim (reasoning items with `encrypted_content`) |
| Effort | `reasoning.effort` (`low`, `medium`, `high`; higher levels map to `high`) |
| Show thinking | `reasoning.summary: "auto"` |

Always sent: `stream: true`, `store: false`, `include: ["reasoning.encrypted_content"]`,
`max_output_tokens`. Tools are `strict` only when every property is required and
`additionalProperties` is `false` (OpenAI's strict-mode rule).

## Reply handling

- `response.output_item.done` completes items: text, function calls (arguments parsed),
  reasoning items (kept verbatim), and any other item type (kept verbatim).
- Stop reason: `ToolUse` if the response contained function calls, `Refusal` if a
  refusal part appeared, otherwise `EndTurn`; `response.incomplete` maps
  `max_output_tokens` → `MaxTokens`, `content_filter` → `Refusal`.
- Usage: `input_tokens` includes cached tokens, so `cache_read = cached_tokens` and
  `input = input_tokens − cached_tokens`.

## Errors and retries

401/403 → `Auth`; 429 → `RateLimited`, except `insufficient_quota`, which is not
retryable (waiting does not refill a quota); 5xx → `Server`; other 4xx →
`InvalidRequest`. Retries use the shared `harness_ports::backoff_delay`.

## Tests

- Unit: body mapping (insta snapshot), decoder, error mapping.
- `tests/http.rs`: wiremock (Bearer auth, retries, quota, cut streams, cancellation, model lookup).
- `tests/replay.rs`: real `gpt-6-luna` recordings from `fixtures/streams/openai/`, including
  sending a recorded tool turn back with its result.
- `examples/record_fixtures.rs`: re-records them (see `fixtures/README.md`).

## Known limits

- `count_tokens` is a local estimate (characters ÷ 4); the usage in each response is exact.
- Capabilities are defaults; `capabilities()` only confirms the model exists.
