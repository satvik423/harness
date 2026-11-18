# harness-runtime

The session actor and the agent loop. Depends on ports, protocol, types and
context only (ADR-001, ADR-016).

## Public API

| Item | Purpose |
|---|---|
| `spawn_session(deps, config)` | Registers a new session, records `SessionOpened`, starts its actor; returns a `SessionHandle` and an event receiver subscribed before the first event |
| `resume_session(deps, config, id)` | Rebuilds a stored session from its events, recovers a task that was running when harness stopped, starts its actor; returns `Resumed { paused, recovered }` |
| `SessionHandle` | `id()`, `send(Op)`, `subscribe()`, `closed()` (waits until the actor has stopped) |
| `RuntimeDeps` | Provider, tools, clock, sandbox, blob store, read ledger, policy, event log, session store, snapshot store (all `Arc<dyn Port>`) |
| `SessionConfig` | Working directory, persona, approval mode, starting permission `mode` and `full_access` (ADR-042), max model calls per task run, output cap, parallel-call limit |
| `ApprovalMode` | `Ask` (someone can answer prompts) or `Never` (headless: asks become denials, `ask_user` gets "no user available") |
| `RuntimeError` | `Closed`, `NotFound`, `Store`, `Corrupt` |
| `OUTCOME_UNKNOWN` | The result given to a call that was running when harness stopped |
| `ModelPrice` | Price per million tokens; `SessionConfig.price` fills `Usage.cost_micros` (ADR-040) |
| `request_history(log, blobs, session)` | Each request as a `RecordedRequest` (event, segment, request), rebuilt from the log (for `harness debug`) |
| `ContextSettings` | When and how to compact: `compact_at_ratio`, `compact_at_tokens`, `compaction_model`, `style` |
| `fork_session(deps, config, parent, at)` | A fork after event `at`, returned as `Forked { handle, events, at, needs_compaction }` (ADR-047) |
| `ModelSource` | Providers and prices for other models, for `/model` inside a session (ADR-046) |
| `recall(log, blobs, session, selector)` | Renders history for a front-end's own `/recall` |

## The loop (one task)

1. Append the user input; emit `UserInput`.
2. Stream the reply. Text and thinking deltas are forwarded live; the assistant
   message is built only from `BlockComplete` events, so it matches the model's
   output exactly.
3. Append the assistant message; emit `AssistantMessage`, `Usage`, `TurnFinished`.
4. If there are tool calls, judge each with the policy, in order:
   - `Allow`: run it. Consecutive parallel-safe, read-only calls run together in a
     `JoinSet`, at most `max_parallel_tools` at once; results keep call order.
   - `Ask`: emit `ApprovalRequested` with the policy's reason and wait. "Session"
     and "always" answers are passed to `Policy::remember` on a blocking thread.
   - `Deny`: the reason becomes the result; a `policy_denied` (or, when only
     approval was missing, `approval_denied`) warning is emitted.
   Unknown tools and failures become error results too. Each result is capped
   once, when recorded (FR-TOOL-04): head 60% + tail 40% of the cap + a footer
   naming the `ToolFinished` event that holds the full output (a blob over 64 KB).
5. Append all results in one user message (I2) and go back to step 2.
6. End as `Completed`, `Failed` (provider error, refusal, output limit),
   `BudgetExhausted`, `Paused` (interrupt) or `Cancelled`.

## Ops handled

`Submit`, `Approve`, `Answer` (for `ask_user`, ADR-028), `Interrupt` (→ Paused at once),
`Pause` (→ Paused at the next safe point), `Resume`, `Cancel` (running or paused),
`Undo` (ADR-034), `Compact { focus }` (a compaction task), `SwitchModel` (compacts, then the next segment uses the new model; ADR-046), `SetMode` (records `ModeChanged` and queues a note for the model; refuses `full` without full access; ADR-042), `Shutdown`. `Fork` answers that it arrives in Phase 5 (ADR-035).

## Permission modes (ADR-042)

Each session holds its `Mode` in a shared cell (`mode.rs`). The policy reads it on every
call, so a switch during a task applies from the next call. Tools reach it through
`ToolCtx::mode` (`propose_plan`). A switch is recorded before it takes effect. Its note
is sent at the start of the next user message while a segment is open. A new segment's
environment states the mode instead, and replay rebuilds both the same way. Approval
requests carry the call's one-line `description` as `summary` (ADR-043).

## Durability, resume and recovery (Phase 3)

- **Journal:** every persisted event is numbered by the session's single writer,
  committed to the event log, then broadcast (ADR-033). The loop waits for the commit
  before the side effect that depends on it: `TurnStarted` before the model call,
  `AssistantMessage` before its tools, `ToolStarted` before a tool runs. If the log
  refuses a write, the task fails before doing anything unsaved.
- **Frame:** opening a segment stores its frozen prefix as a blob referenced by
  `SegmentOpened` (ADR-032); resuming rebuilds the segment from it.
- **Pause:** safe points are before each model call, after each tool (before the next),
  and after an approval decision; pausing while waiting for approval drops the request
  (asked again on resume). A paused task keeps its unanswered round and resumes only
  the calls without results.
- **Recovery (Tech spec 3.14):** a task found `Running`/`AwaitingApproval` becomes
  `Recovering`; calls that started but never finished get `OUTCOME_UNKNOWN` and are
  never re-run; unstarted calls go through the policy again on resume; the task ends
  `Paused` ("recovered"). Edge cases from ADR-036: a task whose final reply was saved
  completes on resume without a model call; a task whose input was never saved is failed.
- **Controls:** while a task is active, the actor takes control requests from the
  session store every 250 ms (ADR-031).
- **Undo:** restores the last turn's snapshotted files, records `UndoApplied` and a
  `SystemNote` sent with the next user message (ADR-034).
- **Attachments:** `InputItem::File` becomes a reference note in the user message
  (path, size, kind; ADR-039).
- **New input while paused** supersedes the paused task (cancelled, its open calls
  answered) (ADR-036).
`Pause`, `Resume`, `Compact`, `SwitchPersona`, `Fork`, `CreateSession` and
`ResumeSession` answer with an `unsupported` warning naming the phase that adds them.

## Design notes

- The actor never blocks: each task runs on its own tokio task, and the actor
  keeps receiving ops while it runs. Cancellation tokens form a tree: session root,
  task, call.
- Interrupting a tool round still appends an error result for every call
  ("not run: the task was interrupted"), so the next input is always valid.
- AGENTS.md and git are read via `spawn_blocking`, off the async workers.
- The runtime holds no rule logic; it only follows `Verdict`s (ADR-029). Tests use
  `harness_testkit::AskForChanges`.
- `ask_user` reaches the user through a per-call `SessionUser` (`UserChannel`):
  the task moves to `AwaitingApproval` until `Op::Answer` arrives.

## Compaction (Phase 5)

`compact.rs` runs one compaction:
1. `CompactionStarted`;
2. the segment's events from the log, rendered;
3. the compaction model (the session's, at effort medium, with structured output);
4. validation;
5. `Compacted` with the record;
6. a new frame and `SegmentOpened`.

On any failure the segment stays and a warning says why.

Compaction is triggered three ways:
- **At a turn boundary**, before new input is added, when the estimate reaches the
  threshold.
- **In an emergency**, after a tool round's results are logged, followed by "Continue
  the task from the summary above."
- **By `Op::Compact`**, run as its own task.

Before each model call, the estimate is the last measured input plus whatever was
appended since. Near the threshold, the provider counts exactly. `memory.rs` keeps the
session's pins and anchors (rebuilt on replay); `history.rs` serves `recall` with full
tool outputs.
