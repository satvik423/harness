# Changelog

All notable changes to harness. Versions follow the release plan in
`building-harness.html` (Part 1.10 and the phase plan).

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
