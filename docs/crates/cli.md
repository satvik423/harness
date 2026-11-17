# harness-cli

The `harness` binary. Wires the adapters into the runtime and hosts the commands.

## Commands

| Command | What it does |
|---|---|
| `harness` | Prints help |
| `harness run PROMPT [--json] [--model provider/model] [--max-turns N] [--approval on-request\|never] [--dangerously-full-access]` | Runs one prompt headless (Anthropic or OpenAI) and exits. `--approval` overrides the profile's approval policy; the `full` profile needs `--dangerously-full-access` |

See the [running guide](../guides/running.md) for setup and examples.

## Library modules

| Module | Purpose |
|---|---|
| `run` | `run_session`: drives a session, renders events, asks approvals, handles Ctrl-C; `anthropic_settings` maps config to adapter settings |
| `render` | `Renderer` (human: answer → stdout, activity → stderr; `--json`: one envelope per line) and `Outcome` (exit codes) |
| `secrets` | `resolve_key(KeyFor::…)`: keyring entry (service `harness`), then the provider's environment variable; errors never show the key and include the `secret-tool` command |
| `wiring` | `build_provider`: Anthropic or OpenAI adapter for the chosen model; `build_policy`: the policy engine from `[policy]`, the rules files and the flags |
| `prompt` | `parse_decision` (approval answers: y, s, a, p, n + reason) and `parse_answer` (question answers: option numbers or free text) |
| `telemetry` | Daily-rotated log file filtered by `HARNESS_LOG` |

## Exit codes of `harness run`

| Code | Meaning |
|---|---|
| 0 | Completed |
| 1 | Error (provider, configuration, missing key) |
| 2 | Turn budget exhausted (`--max-turns`) |
| 3 | The model declined the request |
| 4 | Completed, but actions needing approval were denied (`--approval never`) |
| 130 | Interrupted (Ctrl-C) or cancelled |

## Design notes

- The keyring is read before the async runtime starts, because the keyring client blocks.
- Piped answers are echoed after the prompt (the terminal is not there to do it),
  so result lines start on their own line.
- `todo` calls render as a checklist; `ask_user` questions list numbered options.
- The first Ctrl-C interrupts the task; a second exits immediately with 130.
- `run_session` takes injected streams and dependencies, so tests drive it with
  `FakeProvider` and in-memory buffers.
