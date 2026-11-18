# harness-evals

The `harness-evals` binary: the context-engine evals of Phase 5 (M1 needle, M2
fidelity; Tech spec 3.18). It is not published. Usage, costs and how to read the
results are in [`evals/README.md`](../../evals/README.md).

## Modules

| Module | Purpose |
|---|---|
| `scenario` | Seeded scenarios: facts to plant, filler prose, questions; requirements for fidelity runs (a small xorshift generator, no dependency) |
| `driver` | Drives one real session (runtime, SQLite store in a temp dir, `recall` unless baseline) turn by turn, and collects replies, compactions, records, openers and cost |
| `evals` | `needle` (exact answers after 2+ compactions; `--baseline` is summary-only with no recall) and `fidelity` (pins carried over byte for byte, verbatim in every opener, and I5 over the log) |
| `offline` | `OfflineModel`: a deterministic provider that pins facts it sees when compacting and answers only from its request, so the evals run in the test suite without keys |
| `main` | The CLI; appends a row to `evals/results.tsv` |

## Design notes

- The evals use the real runtime, store and context engine, not mocks. Only the model
  is replaced when offline.
- Offline scores check the harness (do pins survive and reach the next request?). The
  live runs check a real model's judgement.
- Under rule 1 the crate is a binary, so it may use `anyhow`. Its layering rule allows
  everything except the TUI.
