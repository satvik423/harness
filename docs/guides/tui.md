# The terminal UI

```sh
harness                       # open the TUI in this project
harness "fix the login test"  # open it and send a first prompt
harness -c                    # reopen the most recently active session
harness --session 01M7P6      # reopen a stored session (history included)
harness --mode plan           # start in plan mode (read-only, auto, plan, full)
```

The header shows the persona, model, effort, segment, permission mode and the
session's short id; the status bar shows context use, cache hits, cost, the task state
and key hints. A new session's first line names the last one and how to get back to it.

## Keys

| Key | Does |
|---|---|
| Enter | send |
| Alt+Enter, Ctrl+J | new line |
| Esc, Ctrl+C | interrupt the running task (Ctrl+C on an empty, idle input quits) |
| Ctrl+P | pause at the next safe point |
| Shift+Tab | next permission mode: read-only → auto → plan (→ full, only with `--dangerously-full-access`) |
| ↑ / ↓ | earlier prompts in this project (from the first / last line of the input) |
| mouse wheel, PgUp / PgDn, Shift+↑↓ | scroll the transcript (or the open panel) |
| Shift+drag | select text to copy (see below) |
| Ctrl+D (empty input) | quit |

**The mouse.** To get wheel events, harness asks the terminal for mouse reporting, and
then the terminal sends it every mouse event, drags included. So plain dragging no longer
selects text; hold Shift (Option on macOS) to select as usual. To give the mouse back to
the terminal, set this in your config:

```toml
[ui]
mouse = false
```

Finished shell commands keep their last five lines of output on their card.

Paste or drag files into the terminal to attach them (they show as 📎 above the input;
Esc removes them). The model is told where they are and reads them itself.

## Modes

The mode decides what runs without asking: `read-only` (reads only), `auto` (edits in
the project too), `plan` (reads only; the agent proposes a plan for you to approve) and
`full`. Shift+Tab or `/mode` switches it at any time, even while a task runs (it applies
from the next tool call), and the agent is told. In plan mode the plan appears in the
transcript with three choices: approve and auto-accept edits, approve and ask before
each edit, or keep planning (or type what to change). See the
[permissions guide](permissions.md) for the details.

## Approvals and questions

When a tool call needs approval, a panel shows the agent's one-line description of what
the call does (in bold), then the exact command or files, with a diff for file changes.
The description is the agent's claim; judge the command under it (ADR-043). Keys: `o` once, `s` for the session, `a` always, `p` always for this project, `d` deny
(then type a reason for the model, or Esc for none). Questions from the model take a
number, or your own words and Enter. If the terminal is in the background, harness rings
the bell and sends a desktop notification.

## Slash commands

| Command | Does |
|---|---|
| `/help` | commands and keys |
| `/model [provider/model]` | list models to pick from, or name one; this session continues with it (after a compaction) |
| `/effort [low…max]` | show the effort, or continue this session at another one |
| `/mode [read-only\|auto\|plan\|full]` | list the permission modes to pick from, or switch to one (also Shift+Tab) |
| `/compact [focus]` | compact the conversation into a new segment now (see [long sessions](context.md)) |
| `/recall [aN \| words]` | list anchors, or attach an anchor or search result to your next message |
| `/fork [event]` | a new session that shares this one's history up to an event (default: now) |
| `/pause`, `/resume` | pause the task / continue the paused task |
| `/tasks` | this session's tasks |
| `/diff` | files this session changed, as diffs |
| `/undo` | restore the files changed by the last turn |
| `/clear` | start a new session (this one stays saved) |
| `/sessions` | this project's sessions; pick one (↑↓ Enter, or its number) to switch |
| `/cost` | tokens and cost per turn, task and session |
| `/context` | how full the context is |
| `/quit` | leave |

`/model` and `/effort` ask first, then compact the conversation and continue in a new
segment with the new settings; nothing is lost (ADR-046). `/clear` starts a new
session and keeps your current mode.

## Costs

Costs use the `[pricing]` table (US dollars per million tokens). harness ships prices
for the Claude 5.5 models and OpenAI's gpt-6 models (from OpenAI's pricing page,
18 Nov 2026). They change only when harness is updated; nothing is fetched. Your own
entries win, and models without a price show `$?`:

```toml
[pricing."openai/gpt-6-luna"]
input = 0.10
output = 0.50
cache_read = 0.01
cache_write = 0.125
```

These are estimates; your provider's bill is the real number. OpenAI charges more for
prompts over 272K input tokens, which the table does not model; the default context
limit (`context.compact_at_tokens`, 250K) stays below that, and automatic compaction
arrives in Phase 5.

## Seeing what is sent

```sh
harness debug last-request     # the exact body of the last model request (redacted)
harness debug diff-requests    # diff of the last two, with an append-only verdict
```
