# Test fixtures

Recorded provider responses, replayed offline by the provider crates' tests.
They contain response bodies only, never request headers or keys.

| Folder | Recorded with | Re-record |
|---|---|---|
| `streams/openai/` | `gpt-6-luna`, 17 Nov 2026 | `OPENAI_API_KEY="$(secret-tool lookup service harness username openai)" OPENAI_MODEL=gpt-6-luna cargo run -p harness-provider-openai --example record_openai_fixtures` |
| `streams/anthropic/` | not yet recorded (no Anthropic API key) | `ANTHROPIC_API_KEY="$(secret-tool lookup service harness username anthropic)" cargo run -p harness-provider-anthropic --example record_anthropic_fixtures` |

After re-recording, run the replay tests (`cargo test -p harness-provider-openai --test replay`)
and check that nothing sensitive was captured: `grep -rl "sk-\|Bearer" fixtures/`.

## Projects

| Folder | Used by |
|---|---|
| `projects/seeded-bug/` | Phase 2 exit check: a shell project with one planted bug (`sh test.sh` fails until `average()` is fixed). Copied into a temp dir by `harness-cli`'s `fixes_the_seeded_bug_with_fewer_than_three_approvals` test, and used for the live check in `docs/phases/phase-02.md` |
