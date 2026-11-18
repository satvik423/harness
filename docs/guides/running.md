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
- Reading, searching and editing inside the project run without asking (profile
  `auto`). Shell commands ask first, until the sandbox arrives:

  ```
  ▸ shell: find . -name '*.rs' | wc -l
  Allow run `find . -name '*.rs' | wc -l` (commands run without a sandbox)? [y]es once / [s]ession / [a]lways / [p]roject always / [n]o (n <why>):
  ```

  `y` allows it once, `s` for the session, `a` always; `n` (optionally with a reason
  for the model) or closed input denies it. See the [permissions guide](permissions.md).
- When the model asks you a question (`ask_user`), type an option number or your own words.
- Secrets files (`.env`, `*.pem`, `~/.ssh`) are denied to every tool.
- **Ctrl-C** interrupts the task; press it again to exit immediately.

## Options

| Flag | Default | Meaning |
|---|---|---|
| `--model provider/model` | `default_model` from config | `anthropic/…` or `openai/…` |
| `--max-turns N` | 50 | Maximum model calls |
| `--approval on-request\|never` | from `policy.profile` | `never` denies every action that needs approval, without asking |
| `--dangerously-full-access` | off | The `full` profile: no approvals, no protections except deny rules |
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

## Sessions

Each run is saved; the last line shows its session id. Continue it with
`harness run --session ID "…"`, list sessions with `harness session list`, and see the
[sessions guide](sessions.md) for resume, pause, undo and export.

## Current limits (Phase 3)

- Tools: files (`read_file`, `write_file`, `edit_file`, `apply_patch`), search (`glob`,
  `grep`), `shell`, PTY programs (`pty_start`, `pty_write`, `pty_read`, `pty_kill`),
  `todo` and `ask_user`. Web tools arrive later.
- No sandbox yet: commands run as you, which is why they need approval.
- Fork arrives with compaction in Phase 5.
- Text input only; attachments arrive in Phase 11.
