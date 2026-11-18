# ADR-031: Task control between processes, and session locks

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Decision by the user at the start of Phase 3 (P3-T8)

## Context

`harness task pause|resume|cancel ID` must reach a task that may be running in
another terminal. There is no server until Phase 10, and the spec does not say how.

## Decision

- A second migration adds `task_controls (session_id, task_id, control, created_at)`.
  The command writes a request there; the process running the session takes its
  requests every 250 ms while a task is active and applies them (a pause at the next
  safe point). The `SessionStore` port gains `request_control` and `take_controls`.
- Each running session holds an exclusive advisory lock on
  `.harness/locks/<session>.lock` (`flock`, released when the process exits, even by
  `kill -9`). Commands use it to know whether another process runs the session; a
  second process cannot reopen (and so recover) a session that is in use.
- When no process runs the task, the command acts directly: `cancel` reopens the
  session (recovering it if needed) and cancels the paused task; `resume` runs it in
  this process; `pause` explains that a stopped task is recovered as paused.

## Consequences

Control works from any terminal with no extra moving parts, and is testable. Requests
take up to 250 ms to be noticed. The lock file and `locks/` are local state, listed in
`.harness/.gitignore`.
