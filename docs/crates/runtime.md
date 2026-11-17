# harness-runtime

The session actor and the agent loop. Depends on ports, protocol, types and
context only (ADR-001, ADR-016).

## Public API

| Item | Purpose |
|---|---|
| `spawn_session(deps, config)` | Starts an actor; returns a `SessionHandle` and an event receiver subscribed before the first event |
| `SessionHandle` | `id()`, `send(Op)`, `subscribe()` |
| `RuntimeDeps` | Provider, tools, clock, sandbox, blob store, read ledger, policy (all `Arc<dyn Port>`) |
| `SessionConfig` | Working directory, persona, approval mode, max model calls per task, output cap, parallel-call limit |
| `ApprovalMode` | `Ask` (someone can answer prompts) or `Never` (headless: asks become denials, `ask_user` gets "no user available") |
| `BroadcastSink` | Assigns sequence numbers and timestamps under one lock, broadcasts envelopes; `emit_with` builds an event from its own number (ADR-030) |

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

`Submit`, `Approve`, `Answer` (for `ask_user`, ADR-028), `Interrupt` (→ Paused),
`Cancel` (→ Cancelled), `Shutdown`.
`Pause`, `Resume`, `Compact`, `SwitchPersona`, `Fork`, `CreateSession` and
`ResumeSession` answer with an `unsupported` warning naming the phase that adds them.

## Design notes

- The actor never blocks: each task runs on its own tokio task, and the actor
  keeps receiving `Approve`/`Interrupt` while it runs.
- Interrupting a tool round still appends an error result for every call
  ("not run: the task was interrupted"), so the next input is always valid.
- AGENTS.md and git are read via `spawn_blocking`, off the async workers.
- The runtime holds no rule logic; it only follows `Verdict`s (ADR-029). Tests use
  `harness_testkit::AskForChanges`.
- `ask_user` reaches the user through a per-call `SessionUser` (`UserChannel`):
  the task moves to `AwaitingApproval` until `Op::Answer` arrives.
