# ADR-028: `ask_user` events and the `UserChannel` port

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 2 (P2-T8); fills a gap in Tech spec 3.5

## Context

Tech spec 3.5 says `ask_user` "blocks the task (AwaitingApproval-like)" and that
headless mode returns "no user available", but the protocol has no event for a
question and no op for an answer. A tool cannot reach the user by itself: tools are
adapters, and only the runtime talks to front-ends.

## Decision

- `harness-types` gains `QuestionId`.
- `harness-protocol` gains `Event::QuestionAsked { id, task, call_id, question, options, multi }`,
  `Event::QuestionAnswered { id, answer }` and `Op::Answer { request, answer }`.
- `harness-ports` gains the `UserChannel` port (`ask(question, cancel)`) and puts an
  `Arc<dyn UserChannel>` in `ToolCtx` as `user`. `NoUser` answers `AskError::NoUser`.
- The runtime implements it per call: the task moves to `AwaitingApproval`, the question
  is emitted, and the answer arrives through `Op::Answer`. Under `ApprovalMode::Never`
  it answers `NoUser` at once, and the tool tells the model no user is available.

## Consequences

Front-ends render `QuestionAsked` and send `Op::Answer`. `harness run` reads the answer
from stdin; a number picks an option.
