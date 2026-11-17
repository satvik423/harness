# Running harness

## 1. Store an API key (once)

harness reads keys from your OS keyring, so they never sit in a file. Store the
key for the provider you use; `secret-tool` prompts for it:

```sh
# OpenAI
secret-tool store --label='harness openai' service harness username openai
# Anthropic
secret-tool store --label='harness anthropic' service harness username anthropic
```

These match the defaults `providers.openai.api_key = "keyring:openai"` and
`providers.anthropic.api_key = "keyring:anthropic"`. If a keyring entry is
missing, harness falls back to `OPENAI_API_KEY` or `ANTHROPIC_API_KEY`.

## 2. Choose your default model

The default is `anthropic/claude-opus-5-5`. To use OpenAI by default, set it in
`~/.config/harness/config.toml`:

```toml
default_model = "openai/<model-id>"
```

Or pass `--model openai/<model-id>` on each run.

## 3. Run a prompt

```sh
cd my-project
harness run "How many .rs files are here, and which is the largest?"
```

- The answer streams to **stdout**; tool activity (`▸ shell: …`, `✓ …`) goes to **stderr**.
- Reading files runs without asking. Every shell command asks first:

  ```
  ▸ shell: find . -name '*.rs' | wc -l
  Allow: run `find . -name '*.rs' | wc -l`? [y/N]
  ```

  Type `y` to allow it; anything else (or closed input) denies it, and the model is told.
- **Ctrl-C** interrupts the task; press it again to exit immediately.

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--model provider/model` | `default_model` from config | `anthropic/…` or `openai/…` |
| `--max-turns N` | 50 | Maximum model calls |
| `--approval on-request\|never` | `on-request` | `never` denies every action that needs approval, without asking |
| `--json` | off | Print every event as one JSON line on stdout (approval answers are still read from stdin) |

## Exit codes

| Code | Meaning |
|---|---|
| 0 | Completed |
| 1 | Error |
| 2 | Turn budget exhausted |
| 3 | The model declined the request |
| 4 | Completed, but actions needing approval were denied (`--approval never`) |
| 130 | Interrupted |

## Project instructions

Put rules for the agent in `AGENTS.md`. harness reads every `AGENTS.md` from the
git root down to your working directory (up to 32 KiB) and includes them in the
system prompt.

## Logs

Logs go to `~/.local/share/harness/logs/harness.YYYY-MM-DD.log`. Use
`HARNESS_LOG=harness=debug,info` for more detail.

## Current limits (Phase 1)

- Tools: `read_file` and `shell` only. Editing tools arrive in Phase 2.
- No sandbox yet: commands run as you, which is why each one needs approval.
- Sessions are not saved; resume and pause arrive in Phase 3.
- Text input only; attachments arrive in Phase 11.
