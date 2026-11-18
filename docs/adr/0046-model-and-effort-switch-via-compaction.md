# ADR-046: `/model` and `/effort` switch inside a session through compaction

- **Status:** Accepted (updates ADR-038)
- **Date:** 2026-11-18
- **Source:** ADR-038 promised it for Phase 5; the user chose to include it.

## Decision

- **New op:** `Op::SwitchModel { session, model, effort }`.
  - When the session has conversation, the runtime compacts with the current model
    and opens the next segment with the new model and effort. The segment frame
    changes; prompts and tools stay the same.
  - With no conversation yet, the first segment simply opens with them.
- **New event:** `Event::ModelSwitched { model, effort }` records when the switch
  takes effect.
- **Other providers** come through a small runtime trait, `ModelSource`
  (`RuntimeDeps::models`). It returns a provider and the price for a model. The TUI's
  backend implements it with the same provider factory the session uses (keys from
  the keyring), and it runs off the async workers. Without a source, only models of
  the session's own provider can be chosen.
- **`Effort` moves to `harness-types`**, so the protocol can name it; `harness-ports`
  re-exports it.
- **In the TUI**, `/model X` and `/effort X` ask for confirmation, then send the op.
  The header follows `ModelSwitched`. `/clear` still starts a new session.

## Consequences

A long session can move to a cheaper or stronger model without losing its history.
The old segment's thinking blocks are never sent to the new model (guardrail 04).
Costs use the new model's price from the switch on.
