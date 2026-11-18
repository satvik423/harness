# Phase 5: Context engine

## Summary

Long sessions no longer lose details. When the context gets full, harness compacts it
into a new segment:
- a structured summary from the model;
- **pinned facts**, exact quotes that harness checks against their events;
- descriptive **anchors** over every event of the old segment.

The model brings exact details back with the new `recall` tool, and you can attach
them with `/recall`. Nothing already sent is ever edited.

Also in this phase:
- `/compact [focus]`;
- `/model` and `/effort` switching inside a session (they compact first);
- forks (`harness session fork`, `/fork`);
- JSON-schema structured output for both providers;
- a new `harness-evals` binary with the needle (M1) and fidelity (M2) evals.

Offline, the evals give:
- needle: 10/10 answered with anchored compaction, 0/10 with the summary-only
  baseline, over 3 compactions;
- fidelity: 2500/2500 pin checks verbatim over 50 compactions.

**The live runs that M1 and M2 call for are yours to run**, as you chose. The commands
are below and in `evals/README.md`.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P5-T1 Token budgeting | done | `Budget`: `min(ratio × window, compact_at_tokens)`. The estimate is the last measured input plus the appended content; `count_tokens` is called within 10% of the threshold |
| P5-T2 Triggers | done | At a turn boundary before new input; emergency after a logged tool round, then "Continue the task from the summary above."; never while a call waits (I4); manual `Op::Compact` |
| P5-T3 Request and rendering | done | Events rendered with `#seq` on every line, tool results capped, the opener's environment left out; the 3.13 instructions and schema; structured output (ADR-044) |
| P5-T4 Validation | done | I5 exact quotes; I6 anchors clipped, overlaps cut, gaps filled at step boundaries; pins merged, superseded ones switched off; property test |
| P5-T5 Opener | done | Environment, summary from a fixed template, active pins verbatim with `#seq`, anchor index `[[aN \| ev a-b \| tok \| topic; has: …]]` |
| P5-T6 `recall` | done | By anchor, range or search over earlier segments; pages of 2,000 tokens (at most 8,000); full tool outputs; untrusted wrapping; `Recalled` events |
| P5-T7 `/compact`, `/recall`, TUI events | done | `/recall` lists anchors, or attaches an anchor or search result to the next message; compaction notices; the context meter resets |
| P5-T8 Fork | done | `fork_session` reads the parent in place; the fork compacts into its own segment; a fork point inside an open tool round moves back (ADR-047); CLI and `/fork` |
| P5-T9 Needle eval | done (live run pending) | `harness-evals needle [--baseline]`; baseline is summary-only (your choice) |
| P5-T10 Fidelity eval | done (live run pending) | `harness-evals fidelity`: 50 compactions, carry-over and opener checks, I5 |
| P5-T11 Anthropic preserved-thinking smoke test | written, not run | `crates/provider-anthropic/tests/live.rs`, opt-in with `--ignored`; there is no Anthropic key |
| ADR-038 follow-through | done | `/model` and `/effort` inside a session (ADR-046, your choice) |

## Requirements covered

- **FR-CTX-03:** anchored compaction with a summary, verbatim pins and descriptive
  anchors.
- **FR-CTX-04:** `recall` by anchor, range or query, with token-bounded pages.
- **FR-TOOL-04:** the trimmed-output footer now works: recall by range returns the full
  output.
- **FR-SES-06:** fork at any event.
- **M1 and M2:** the eval harness is built; the live numbers are pending your runs.

## Exit criteria

| Criterion | Evidence |
|---|---|
| Needle accuracy ≥ 95% (M1), clearly above the baseline | **Offline:** `harness-evals needle --offline` gives 10/10 (100%) over 3 compactions; `--baseline` gives 0/10. Rows are in `evals/results.tsv`, and the same checks run as tests (`evals::tests`). **Live: pending your run:** `harness-evals needle --model openai/gpt-6-luna` and `--baseline` (about $0.10 each) |
| Pinned fidelity 100% (M2) | **Offline:** `harness-evals fidelity --offline` gives 2500/2500 pin checks verbatim, 50 compactions, 50/50 requirements pinned. **Live: pending your run:** `harness-evals fidelity --model openai/gpt-6-luna` (about $0.03) |
| Invariant tests pass across compactions | New `check_segments` (I1–I2 per segment) and `check_events` (I4–I6) run in the runtime tests: turn-boundary, emergency, manual, resume, fork and switch paths. I1–I4 were not weakened |
| Release v0.3 | After your approval and the live evals: tag `v0.3.0` |

**Live in tmux** (gpt-6-luna, about $0.002 in total):
1. I planted a codename and a Postgres version, then ran `/compact`. The result: "◇
   compacted segment 0 → 1 · 1 anchors · 6 pinned facts".
2. A question needing the facts was answered from the pins, and the model called
   `recall {"anchor":"a1"}` on its own to quote the first message.
3. After quitting, `harness -c` reopened the session in segment 1.
4. `/recall a1` attached the events (↺), and the reply used them.
5. `/effort medium` asked, then compacted into segment 2 ("now using
   openai/gpt-6-luna · effort medium"). The pins were carried over.
6. `/fork` opened a new session, which answered from the same pins.
7. `harness debug diff-requests`: "append-only: yes" within segment 1.

The live run found three problems, all fixed:
- `diff-requests` compared requests from different segments. It now compares within
  one segment only.
- The recall card showed raw JSON.
- The opener's environment block was repeated in the rendered history.

## Tests added (counts per crate) and coverage

728 tests (up from 674, 54 new), 63 doctests (+5), 1 opt-in live test skipped.

| Crate | New tests |
|---|---|
| context | +16 (budget, estimates, rendering, schema, validation and the I6 property test, opener, request) |
| runtime | +12 (manual, boundary and emergency compaction, failure, recall, resume, fork ×2, model switch ×3) |
| evals | +6 (scenarios, offline model, offline needle and fidelity) |
| cli | +5 (context settings, fork, compaction output, `diff-requests` segments, backend fork) |
| tui | +5 (compaction notices and meter, `/compact` and `/recall`, recall attachments, `/fork`, recall cards) |
| tools | +4 (`recall`) |
| store-sqlite | +2 (projections, forks) |
| provider-anthropic, provider-openai | +2, +1 (structured output; live test) |
| ports, types | +1, +1 |

Coverage (`cargo llvm-cov`, lines): **90.1%** in total.

| Crate | Coverage |
|---|---|
| runtime | 87.2% |
| context | 97.5% |
| policy | 93.2% |
| tools | 95.8% |
| store-sqlite | 92.7% |
| tui | 82.7% |
| cli | 88.4% |
| evals | 58.6% (its CLI entry point is run by hand) |

## New dependencies

No new external crates. Changes inside the workspace:
- `harness-context` now uses `harness-protocol` (allowed by the rules) and `proptest`
  as a dev-dependency;
- the new `harness-evals` uses only workspace crates;
- `harness-runtime` enables tokio's `fs` feature. It always needed it (attachments,
  Phase 4) but only compiled in the full workspace. Every crate now builds on its own.

## Decisions / ADRs

- **ADR-044:** structured output brought forward from Phase 15 (your choice).
- **ADR-045:** the validated record travels in `Compacted`; the store projects anchors
  and pins (migration 0003).
- **ADR-046:** `/model` and `/effort` switch inside a session through compaction. It
  adds `Op::SwitchModel`, `ModelSwitched` and `ModelSource`, and moves `Effort` into
  `harness-types`. ADR-038 is marked superseded.
- **ADR-047:** a fork reads its parent in place and starts from a compaction; fork
  points move out of open tool rounds.

## Deviations from the plan (and user approval)

- **Needle baseline:** summary-only compaction, not server compaction. The plan
  contradicted itself (M1 says summary-only); you chose summary-only.
- **Live evals:** run by you, not by me (your choice). Offline runs and tests prove
  the harness logic.
- **Structured output** moved from Phase 15 to Phase 5 (your choice, ADR-044).
- **`/model` and `/effort` inside a session** are now in Phase 5 (your choice,
  ADR-046).
- **Fork:** compacts whenever there is conversation to carry, not only mid-segment
  (ADR-047; the plan I posted said so, and you said OK).

## Known issues

- **The M1 and M2 live numbers are pending** your runs.
- **Anthropic** structured output and preserved thinking are tested only against
  recorded bodies; the live test needs a key.
- **Pricing:** a compaction priced with a different `compaction_model` is billed at
  the session model's price.
- **Fork timing:** if harness is killed between creating a fork and finishing its first
  compaction, resuming the fork may treat a task inherited from the parent as
  interrupted.
- **Pin quality:** pins are only as good as the model's choice. The fidelity eval
  checks that pins stay exact, not that the right facts were pinned.
- **Carried from Phase 4:** the policy-denied reason still shows twice; tmux forwards
  no focus or OSC 9 events.

## Notes for later phases

- **Phase 6:** `/persona` switching uses the same compaction path as `SwitchModel`.
- **Phase 7:** PreCompact and PostCompact hooks fit around `compact.rs` (CompactionStarted
  and Compacted are where they go).
- **Phase 12:** fallback models can reuse `ModelSource`.
- **Phase 14:** auto-recall can search with `History`, and `Recalled { auto: true }`
  exists.
- **Phase 15:** the JSON-RPC server needs a `fork` method that returns the new session.
