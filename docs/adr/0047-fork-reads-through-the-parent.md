# ADR-047: A fork reads its parent in place and starts from a compaction

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 5 (FR-SES-06, ADR-035, Tech spec 3.14)

## Decision

- **A fork is a new session.** `harness_runtime::fork_session(deps, config, parent,
  at)` creates it with `parent_id` and `forked_at_seq`. It is not an op on the
  parent's actor: a front-end creates and owns the new session.
- **Nothing is copied.** The store reads a fork's events from its lineage: each
  ancestor's events up to the point its child forked, then the fork's own. This works
  for `range`, `last_seq`, search, `recall` and replay, through any number of forks.
  The fork's own numbering continues after `at`.
- **The fork continues in its own segment.** When the inherited segment holds
  conversation, the front-end sends `Op::Compact`, and the fork starts from a summary
  of the parent's segment up to `at`.
  - **Change from the spec:** the spec asks for compaction only when `at` falls
    mid-segment. Here a fork compacts whenever there is conversation to carry, so it
    never reuses the parent's thinking blocks. A segment with no messages yet needs
    no compaction.
- **Fork points are moved out of open tool rounds.** A fork point inside an
  unanswered tool round moves back to just before that round's assistant message, so
  a fork never inherits a call that waits for a result (I4). `Forked::at` reports the
  point used.
- **Front-ends:** `harness session fork ID [--at SEQ]` and `/fork [event]`. The
  default is the parent's last event.
- **Deletion:** a parent with forks still cannot be deleted (ADR-041).

## Consequences

Forks are cheap: one compaction, no copying. A fork depends on its parent's rows,
which is why the parent cannot be deleted while forks exist.
