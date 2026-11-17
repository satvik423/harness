# Test fixtures

Recorded provider responses, replayed offline by the provider crates' tests.
They contain response bodies only, never request headers or keys.

| Folder | Recorded with | Re-record |
|---|---|---|
| `streams/openai/` | `gpt-6-luna`, 17 Nov 2026 | `OPENAI_API_KEY="$(secret-tool lookup service harness username openai)" OPENAI_MODEL=gpt-6-luna cargo run -p harness-provider-openai --example record_fixtures` |
| `streams/anthropic/` | not yet recorded (no Anthropic API key) | `ANTHROPIC_API_KEY="$(secret-tool lookup service harness username anthropic)" cargo run -p harness-provider-anthropic --example record_fixtures` |

After re-recording, run the replay tests (`cargo test -p harness-provider-openai --test replay`)
and check that nothing sensitive was captured: `grep -rl "sk-\|Bearer" fixtures/`.
