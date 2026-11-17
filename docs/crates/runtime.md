# harness-runtime

The session actor and the agent loop. Depends on ports, protocol, types and
context only (ADR-001, ADR-016).

## Public API

| Item | Purpose |
|---|---|
| `spawn_session(deps, config)` | Starts an actor; returns a `SessionHandle` and an event receiver subscribed before the first event |
| `SessionHandle` | `id()`, `send(Op)`, `subscribe()` |
| `RuntimeDeps` | Provider, tools, clock, sandbox, blob store, read ledger (all `Arc<dyn Port>`) |
| `SessionConfig` | Working directory, persona, approval mode, max model calls per task |
| `ApprovalMode` | `Ask` (prompt for every non-read-only call) or `Never` (deny without asking) |
| `BroadcastSink` | Assigns sequence numbers and timestamps, broadcasts envelopes |

## The loop (one task)

1. Append the user input; emit `UserInput`.
2. Stream the reply. Text and thinking deltas are forwarded live; the assistant
   message is built only from `BlockComplete` events, so it matches the model's
   output exactly.
3. Append the assistant message; emit `AssistantMessage`, `Usage`, `TurnFinished`.
4. If there are tool calls, run them in order: read-only calls run directly,
   others ask for approval. Unknown tools, denials and failures become error results.
5. Append all results in one user message (I2) and go back to step 2.
6. End as `Completed`, `Failed` (provider error, refusal, output limit),
   `BudgetExhausted`, `Paused` (interrupt) or `Cancelled`.

## Ops handled in Phase 1

`Submit`, `Approve`, `Interrupt` (→ Paused), `Cancel` (→ Cancelled), `Shutdown`.
`Pause`, `Resume`, `Compact`, `SwitchPersona`, `Fork`, `CreateSession` and
`ResumeSession` answer with an `unsupported` warning naming the phase that adds them.

## Design notes

- The actor never blocks: each task runs on its own tokio task, and the actor
  keeps receiving `Approve`/`Interrupt` while it runs.
- Interrupting a tool round still appends an error result for every call
  ("not run: the task was interrupted"), so the next input is always valid.
- AGENTS.md and git are read via `spawn_blocking`, off the async workers.
- Phase 1 treats "allow for session" and "always" as one-off grants; remembered
  grants arrive with the policy engine in Phase 2.
