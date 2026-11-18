# Changelog

All notable changes to harness. Versions follow the release plan in
`building-harness.html` (Part 1.10 and the phase plan).

## Unreleased (Phase 5)

- **Long sessions keep every detail:** when the context gets full, harness compacts it
  into a new segment with a structured summary, pinned facts (exact quotes, checked)
  and anchors over every event. `/compact [focus]` does it now.
- **`recall`:** the model fetches earlier events by anchor, event range or search, in
  pages; a trimmed tool result's full output comes back by range. `/recall` lists
  anchors or attaches one to your next message.
- **`/model` and `/effort` switch inside a session** (the conversation is compacted
  first), across providers too.
- **Forks:** `harness session fork ID [--at SEQ]` and `/fork [event]`.
- **Structured output** for OpenAI and Anthropic (used by compaction).
- **Evals:** `harness-evals needle` and `fidelity` (M1, M2), offline or live; results
  in `evals/results.tsv`.
- `harness debug diff-requests` compares requests of one segment.

## Phase 4

- **Terminal UI:** `harness` opens it; `harness --session ID` reopens a session with its
  history. Streaming Markdown with highlighted code, tool cards with live output and
  diffs, approval panel with diff previews and shortcuts, questions, file attachments by
  paste or drag, slash commands (`/help /model /effort /pause /resume /tasks /diff /undo
  /clear /cost /context /quit`), status bar with context, cache, cost and task state, and
  a notification when the terminal is in the background.
- **Costs** per turn, task and session from the `[pricing]` table.
- **`harness debug last-request` and `diff-requests`:** the exact request bodies and
  the append-only check.
- **Getting back to sessions:** `harness -c` opens the most recently active session;
  `/sessions` lists them and switches; a new session names the last one; the header
  shows the session id. Sessions nobody typed into are not kept.
- **`harness session delete ID`** (asks first, or `--yes`); unused stored files are freed.
- **TUI:** ↑/↓ prompt history (kept across restarts), mouse-wheel scrolling
  (`[ui] mouse = false` to turn off), shell cards keep their last output lines, a model
  picker in `/model`, faster redraws in long sessions.
- **Prices for OpenAI's gpt-6 models** in the built-in pricing table.
- **Approvals explain themselves:** commands come with a one-line description from the
  agent, shown above the exact command (ADR-043).
- **Permission modes** `read-only`, `auto`, `plan`, `full` switch at any time: Shift+Tab,
  `/mode`, `--mode`, or `policy.profile`. In plan mode the agent explores and proposes a
  plan (`propose_plan`); edits wait for your approval (ADR-042).

## 0.1.0 — "It works" (2026-11-18)

The first release: a durable, headless coding agent.

### Highlights

- **Headless agent loop** (`harness run`) with Anthropic (Messages API) and OpenAI
  (Responses API) providers, API keys from the OS keyring or environment variables.
- **Tools:** `read_file`, `write_file`, `edit_file`, `apply_patch`, `glob`, `grep`,
  `shell`, `pty_start` / `pty_write` / `pty_read` / `pty_kill`, `todo`, `ask_user`.
  Output is capped once when recorded, with a recall handle to the full output.
- **Permissions:** profiles (`read-only`, `auto`, `ci`, `full`), `rules.toml` with
  allow / ask / deny on tool, command prefix and path, shell commands judged part by
  part, approvals once / session / always / deny-with-reason, protected secrets paths.
- **Sessions that survive anything:** every event is committed to
  `.harness/state.db` before harness acts on it. Sessions can be listed, shown,
  continued, exported (Markdown, JSONL) and undone; tasks can be paused (also from
  another terminal) and resumed after a restart; a crash or `kill -9` leaves a task
  that is recovered as paused, with interrupted tool calls reported, never re-run.
- **Undo:** file snapshots before every write; `harness session undo ID` restores the
  last turn's files and tells the model.

### Verified

- 200 of 200 random `kill -9` runs recover with valid transcripts (800 over four seeds).
- A paused task continues correctly after a process restart.
- A seeded bug is fixed by a live model with one approval; `**/.env*` is never readable.
- `apply_patch` survives 10 minutes of fuzzing.

### Not yet

Terminal UI (Phase 4), compaction, recall and fork (Phase 5), personas (Phase 6),
hooks and skills (Phase 7), sandbox (Phase 13). See the phase plan.

Phase reports: `docs/phases/phase-00.md` to `phase-03.md`.
