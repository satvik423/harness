# harness-cli

The `harness` binary. Wires the adapters into the runtime and hosts the commands.

## Commands

| Command | What it does |
|---|---|
| `harness [PROMPT] [-c \| --session ID] [run flags]` | Opens the terminal UI in a new session, the most recently active one (`-c`), or a stored one (prints help when not on a terminal) |
| `harness run PROMPT [--session ID] [--json] [--model provider/model] [--max-turns N] [--approval on-request\|never] [--dangerously-full-access]` | Runs one prompt headless in a new session (or an existing one) and exits |
| `harness session list` | Sessions of this project, newest first, with their last task state |
| `harness session show ID` | A session's tasks and events |
| `harness session resume ID [run flags]` | Continues the paused (or recovered) task |
| `harness session export ID [--format md\|jsonl] [--output FILE]` | Writes the session (default `.harness/exports/ID.md`) |
| `harness session undo ID` | Restores the files changed by the last turn |
| `harness session delete ID [--yes]` | Deletes the session and everything stored for it, after a `[y/N]` (ADR-041) |
| `harness task list [--session ID]` | Tasks with state and reason |
| `harness debug last-request\|diff-requests [--session ID]` | The exact provider body of the last request (redacted), or the diff of the last two with an append-only verdict |
| `harness task pause\|resume\|cancel ID` | Leaves a request for the process running the session, or acts directly when none does (ADR-031) |

Ids accept any unique prefix. See the [sessions guide](../guides/sessions.md).

## Library modules

| Module | Purpose |
|---|---|
| `run` | `execute(options, deps, persona, Target, Action, locker, io, interrupt)`: opens a new or stored session (taking its lock), performs a prompt, resume, cancel or undo, renders events, answers prompts from a background stdin reader (a pause elsewhere drops a waiting prompt), handles Ctrl-C; `run_session` for a plain prompt |
| `render` | `Renderer` (human: answer → stdout, activity → stderr; `--json`: one envelope per line) and `Outcome` (exit codes) |
| `secrets` | `resolve_key(KeyFor::…)`: keyring entry (service `harness`), then the provider's environment variable; errors never show the key and include the `secret-tool` command |
| `wiring` | `build_provider`; `build_policy`; `session_frame` / `session_model` (a stored session's frame and model); `Offline` (provider for commands that never call a model) |
| `tui` | `CliBackend`: the TUI's `Backend` over the runtime and store. Providers are built on a blocking thread through an injectable `Providers` function (`with_providers` in tests); a session the TUI leaves is deleted if nobody typed into it; prompt history comes from `recent_inputs` |
| `factory` | `SessionFactory`: policy, tools, store and price for a session; shared by `run` and the TUI |
| `debug` | `body` (adapter-built JSON), `redact`, `diff_report` |
| `sessions` | `session_table`, `task_table`, `event_line`, `export_markdown`, `export_jsonl`, `resolve_session` / `resolve_task` (unique prefixes), `format_time`, `recent_sessions` (sessions with a prompt, most recently active first) |
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
