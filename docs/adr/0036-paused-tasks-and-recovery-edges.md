# ADR-036: Paused tasks, new input, and recovery edge cases

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 3 (P3-T4, P3-T6); found by the kill -9 harness

## Context

Interrupt leaves a task paused (SDD 2.8), and a crash leaves one recovered as paused.
The spec does not say what new input does to a paused task, and the kill -9 runs found
two states the recovery algorithm does not mention.

## Decision

- A session has at most one active (running or paused) task. New input while a task
  is paused supersedes it: the paused task is cancelled, its unanswered calls get
  "not run: the task was cancelled", then the new task starts. This keeps "interrupt,
  then type something else" working. `harness run --session` refuses instead, so a
  recovered task is never dropped by accident from the command line.
- A paused task keeps its unanswered tool round; resuming runs only the calls without
  a result, in order, and appends all results in one message (I2).
- If the harness stopped after the final reply was recorded but before the task was
  marked completed, resuming completes it without calling the model.
- If it stopped after the task started but before its input was saved, recovery marks
  the task failed ("submit it again") instead of paused: there is nothing to continue.
- The actor collects a task that is recording its final state before handling the
  next op, so a prompt sent the moment a task completes is not refused as busy.

## Consequences

Recovery always leaves a session that accepts input. 800 kill -9 runs over four seeds
recover with valid transcripts.
