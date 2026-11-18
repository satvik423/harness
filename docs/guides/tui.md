# The terminal UI

```sh
harness                       # open the TUI in this project
harness "fix the login test"  # open it and send a first prompt
harness --session 01M7P6      # reopen a stored session (history included)
```

The header shows the persona, model, effort, segment and permission profile; the status
bar shows context use, cache hits, cost, the task state and key hints.

## Keys

| Key | Does |
|---|---|
| Enter | send |
| Alt+Enter, Ctrl+J | new line |
| Esc, Ctrl+C | interrupt the running task (Ctrl+C on an empty, idle input quits) |
| Ctrl+P | pause at the next safe point |
| PgUp / PgDn, Shift+↑↓ | scroll the transcript |
| Ctrl+D (empty input) | quit |

Paste or drag files into the terminal to attach them (they show as 📎 above the input;
Esc removes them). The model is told where they are and reads them itself.

## Approvals and questions

When a tool call needs approval, a panel shows what it would do, with a diff for file
changes: `o` once, `s` for the session, `a` always, `p` always for this project, `d` deny
(then type a reason for the model, or Esc for none). Questions from the model take a
number, or your own words and Enter. If the terminal is in the background, harness rings
the bell and sends a desktop notification.

## Slash commands

| Command | Does |
|---|---|
| `/help` | commands and keys |
| `/model [provider/model]` | show models, or pick one and start a new session with it |
| `/effort [low…max]` | show or pick the effort for a new session |
| `/pause`, `/resume` | pause the task / continue the paused task |
| `/tasks` | this session's tasks |
| `/diff` | files this session changed, as diffs |
| `/undo` | restore the files changed by the last turn |
| `/clear` | start a new session (this one stays saved) |
| `/cost` | tokens and cost per turn, task and session |
| `/context` | how full the context is |
| `/quit` | leave |

A session keeps the model it started with; `/model` and `/effort` start a new one
(compaction in Phase 5 will allow switching inside a session).

## Costs

Costs use the `[pricing]` table in your config (US dollars per million tokens). Models
without a price show `$?`:

```toml
[pricing."openai/gpt-6-luna"]
input = 1.25
output = 10.0
cache_read = 0.125
```

## Seeing what is sent

```sh
harness debug last-request     # the exact body of the last model request (redacted)
harness debug diff-requests    # diff of the last two, with an append-only verdict
```
