# ADR-043: Approvals show a one-line description written by the model

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 4 trial follow-up 2. The user could not judge commands they did not
  understand from the command text alone.

## Context

The approval panel showed only the policy's reason, for example
``run `find . -name '*.rs' -newer Cargo.lock | xargs wc -l` ``. Only the model knows why
it wants to run a command. Asking a second model to explain it would cost a call per
approval and could also be wrong.

## Decision

- `shell`, `pty_start` and `pty_write` have a required `description` field: *one short
  line in plain words for the user deciding whether to approve*. The tools ignore it when
  they run.
- `Event::ApprovalRequested` gains `summary: Option<String>`: the call's `description`
  input, cut to one line of at most 120 characters. Any tool with a `description` string
  gets one, so MCP tools that ask for it will too. Old logs without the field still load.
- Front-ends show the summary first and the exact call **under it, always**: the TUI
  panel (bold summary, then the reason with the command), the headless prompt, the
  notification, and `harness session show`.

## Consequences

Approvals are quicker to judge. The description is the model's claim, not a verified
fact: a prompt injection can make it lie. That is why the exact command is always shown
next to it, and why rules and remembered approvals keep matching the command, never the
description. Sessions started before this change keep their frozen tools, so their
approvals show the command only.
