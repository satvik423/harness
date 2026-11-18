# Phase 03: Store & sessions

## Summary

Nothing is ever lost. Every persisted event is committed to `.harness/state.db`
before it is broadcast and before harness does anything that depends on it.

What now works:

- Sessions can be listed, shown, continued, exported and undone.
- Tasks pause at safe points, also when the request comes from another terminal, and
  resume after a process restart.
- A task that was running when the process died (even by `kill -9`) is recovered as
  paused. Interrupted tool calls are reported to the model as "outcome unknown" and
  never re-run.

All three exit criteria are met:

- 200 of 200 random-kill runs recover with valid transcripts; 800 over four seeds.
- A paused task continues correctly after a process restart, both scripted and live.
- Release v0.1: version 0.1.0, `CHANGELOG.md`, local tag `v0.1.0`.

The kill -9 harness found three real bugs. Each was fixed test-first.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P3-T1 | Done | New `harness-store-sqlite`:<br>• migrations from 3.12 plus `0002_task_controls`, versioned with `PRAGMA user_version`<br>• WAL, `synchronous = NORMAL`<br>• one writer thread, batched transactions with a savepoint per job, acknowledged after commit<br>• read connection on the blocking pool<br>• views updated in the same transaction<br>• FTS5 search |
| P3-T2 | Done | Blob files in `.harness/blobs/ab/cdef…`, written via temp file + rename, checked against their hash on every read, metadata in `blobs`. Events keep referring to blobs by hash (tool output over 64 KB, segment frames, snapshots) |
| P3-T3 | Done | `resume_session` rebuilds the current segment from its frozen frame (ADR-032), then user input, replies and tool results. It also rebuilds task states and a partially answered tool round |
| P3-T4 | Done | Task manager:<br>• pause flag checked at the three safe points<br>• `Op::Resume`, also after a restart<br>• cancel of running or paused tasks<br>• cancellation-token tree (session, task, call)<br>• at most one active task; new input supersedes a paused one (ADR-036)<br>• control requests from other processes every 250 ms (ADR-031) |
| P3-T5 | Done | Interrupt aborts the stream (`AssistantAborted`, not in the transcript) and kills the tool's process group ("interrupted by user"); every unrun call gets an error result |
| P3-T6 | Done | The recovery algorithm of 3.14, plus two edge cases (ADR-036). Kill -9 harness: the test binary re-runs itself as a child, kills it with SIGKILL at a random acknowledged event (seeded), then recovers and checks |
| P3-T7 | Done | File tools snapshot before writing (`ToolCtx::snapshots`; no snapshot, no write). `Op::Undo` restores the last turn's files, deletes files the turn created, records `UndoApplied`, and tells the model with a note on the next user message (ADR-034) |
| P3-T8 | Done | `harness run --session`; `session list/show/resume/export/undo`; `task list/pause/resume/cancel`; unique id prefixes. Fork moved to Phase 5 (ADR-035, user decision) |
| P3-T9 | Done | `.harness/.gitignore` (`state.db*`, `blobs/`, `exports/`, `locks/`) is written when the store first opens |

## Requirements covered

| Requirement | How |
|---|---|
| FR-SES-01 | Sessions in `.harness/state.db`; `session list/show/resume/export` |
| FR-SES-02 | Interrupt (Ctrl-C, `Op::Interrupt`) and cancel (`task cancel`, `Op::Cancel`), for running and paused tasks |
| FR-SES-03 | Pause at the next safe point (`Op::Pause`, `task pause` from any terminal) and resume after a restart |
| FR-SES-04 | Crash recovery: acknowledged events never lost; interrupted calls reported, never re-run (800 kill -9 runs) |
| FR-SES-05 | Snapshots before every file-tool write; `session undo` |
| FR-SES-06 | Moved to Phase 5 with compaction (ADR-035, user decision) |
| FR-CTX-01 | Every event is stored: inputs, replies (raw provider blocks), full tool outputs, approvals, notes |
| FR-IN-03 | Content-addressed blob store; events refer to blobs by hash |
| NFR-03 | Commit before broadcast and before dependent side effects (the journal, ADR-033); a refused append stops the task first |

## Exit criteria

1. **200 of 200 random-kill runs resume with valid transcripts.**
   - Command: `HARNESS_CRASH_RUNS=200 cargo test -p harness-cli --test crash_recovery random -- --nocapture`
   - Result: `200 random-kill runs recovered (seed 1592642302, 199 events per full run)`.
   - Also run with seeds 424242, 7 and 99: 200 of 200 each, so 800 kills in total.
   - Each run checks:
     - every acknowledged event is in the log, and the numbering is gapless;
     - the session rebuilds and recovers;
     - a resumed task completes;
     - a new prompt produces a valid request (invariants I1, I2, no prefill over every request);
     - no `exec` call ran twice.
   - The normal test suite runs 20 kills (seeded, so failures can be replayed).
2. **Pause, then a process restart, then resume continues correctly.**
   - Scripted: `pause_then_process_restart_then_resume_continues_the_task`. A child process pauses at a safe point and exits; a new process resumes. Finished calls are not re-run, the remaining ones run, the transcript is valid, and the task completes.
   - In-process: `pause_takes_effect_after_the_running_tool_and_resume_after_restart_runs_the_rest`.
   - Live with gpt-6-luna:
     - A `harness run` executed `sleep 6 && echo first`.
     - From another process, `harness task pause` left a request.
     - The task paused after the sleep, before the next call (exit 130, "paused. Continue with: harness session resume …").
     - `harness session resume` in a new process ran `echo second` and answered with both outputs. The task ended `completed`.
3. **Release v0.1:** workspace version 0.1.0, `CHANGELOG.md`, annotated local tag `v0.1.0` (user decision; not pushed).

Also checked live (gpt-6-luna):

- `harness run` created `hello.txt` and saved session `01M7P3WE…`.
- `session list` showed it.
- `session undo` removed the created file.
- `session export` wrote 25 events.
- `run --session` continued the conversation with its history.

Checks (all pass):

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 568 passed |
| `cargo test --workspace --doc` | 46 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 94.0% lines |

## Tests added and coverage

| Crate | Tests (total) | Line coverage |
|---|---|---|
| harness-runtime | 53 (6 unit, 34 session, 13 persistence) | **89.8%** (target 80%) |
| harness-context | 35 | **97.7%** (target 80%) |
| harness-policy | 41 | **92.9%** (target 80%) |
| harness-store-sqlite (new) | 22 (8 unit, 14 integration) | 90.9% |
| harness-cli | 63 (47 unit, 12 binary, 3 crash/pause, 1 telemetry) | 90.7% |
| harness-tools | 127 | 96.1% |
| harness-testkit / ports | 32 / 31 | 93.7% / 95.3% |
| providers (anthropic / openai) | 42 / 37 | 94.7% / 95.3% |
| config / types / protocol | 46 / 32 / 7 | 97.4% / 97.4% / 100% |
| **Total** | **568** + 46 doctests (Phase 2: 512 + 41) | **94.0%** |

## New dependencies

| Crate | Version | Why |
|---|---|---|
| `rusqlite` | 0.43.0 (`bundled`) | The SQLite event log and catalogue. The bundled build includes FTS5 (tested). Its `libsqlite3-sys` 0.40.0 brings no new licences |
| `nix` feature `fs` | 0.32.0 (already used) | `flock` for the session lock |

No RustSec advisories (`cargo deny` passes). Crates moved into existing crates:

- normal dependencies: `async-trait` (runtime, cli), `tokio-util` (cli), `blake3` (testkit), `serde` (context);
- dev-dependencies: `tokio` (testkit), `serde_json` (cli tests).

## Decisions / ADRs

- **ADR-031** Task control between processes (requests in `state.db`, polled at safe points) and session locks. User decision.
- **ADR-032** A segment's frozen prefix is stored as a frame blob referenced by `SegmentOpened`. A resumed session keeps its provider and model.
- **ADR-033** The session's writer (journal) numbers events and commits before broadcasting. `EventLog::append` takes a numbered `LoggedEvent` and enforces gapless numbers. `EventSink::emit_with` is removed (supersedes ADR-030's mechanism).
- **ADR-034** Snapshot ports (`FileSnapshots` in `ToolCtx`, `SnapshotStore`), `Op::Undo`, and the "turn" defined as the assistant message whose calls ran.
- **ADR-035** Fork moves to Phase 5 with compaction. User decision.
- **ADR-036** At most one active task; new input supersedes a paused task (the CLI refuses instead); recovery edge cases.
- Smaller choices:
  - `harness run` prints the session id at the end.
  - Read-only commands (`session list/show/export`, `task list/pause`) never create `.harness/` in a folder that has none.
  - The repository's own `.gitignore` ignores every `.harness/` folder.
  - A successful `task cancel` exits 0.
  - Recovery leaves at most the latest task active.

## Deviations from the plan (and user approval)

| Change | Approval |
|---|---|
| Fork (FR-SES-06) moved from Phase 3 to Phase 5 (ADR-035) | Approved by the user at the start of the phase |
| Task control through requests in `state.db`, plus session locks (ADR-031); fills a gap in 3.16 | Approved by the user at the start of the phase |
| Release v0.1 means version 0.1.0, `CHANGELOG.md` and a local tag (packaging stays in Phase 15) | Approved by the user at the start of the phase |

ADR-032, -033, -034 and -036 fill gaps the spec leaves open without changing anything it states.

## Known issues

- Rebuilding reads all of a session's events. Fine for now; Phase 5 segments let it start at the current segment.
- Control requests take up to 250 ms to be noticed.
- `synchronous = NORMAL`: a power loss (not a killed process) can lose the last commits. This is a config candidate.
- Shell commands are not snapshotted for undo (ADR-034).
- The `tokens` column of `events` is not filled yet (Phase 5 needs it).
- Queued tasks (the `Queued` state with a scheduler) are not used yet: one task at a time per session.

## Notes for later phases

- **Phase 4 (TUI):**
  - Render `UndoApplied` and recovery warnings.
  - Offer `/undo`, `/pause`, `/resume` (the ops exist).
  - Pending prompts must be dropped when a task leaves `AwaitingApproval` (the CLI shows how).
- **Phase 5:**
  - Fork (ADR-035).
  - Rebuild from the current segment only.
  - Fill `events.tokens`.
  - `recall` reads the footer's `{"seq_from","seq_to"}` from the log (inline text or blob).
- **Phase 7:** `TaskPaused` / `TaskResumed` hooks go at the transitions this phase built.
- **Phase 8:** subagents use the `Queued` state and the cancellation-token tree (session → task → call).
- **Phase 10:** the server can replace the control mailbox with direct ops; keep the mailbox for CLI use.
