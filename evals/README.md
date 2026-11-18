# Context-engine evals

Phase 5's exit criteria are measured here (plan Part 1 metrics M1 and M2, Tech
spec 3.18). The runner is the `harness-evals` binary (`crates/evals`). It drives
real harness sessions (runtime, SQLite store, `recall`) turn by turn, the way a
user would, on a temporary store.

| Eval | Metric | Target |
|---|---|---|
| `needle` | Facts planted at the start are answered exactly at turn ~200, after 2 or more compactions | ≥ 95%, clearly above the summary-only baseline (M1) |
| `needle --baseline` | The same run with summary-only compaction: no pinned facts, no anchors, no `recall` | the comparison for M1 |
| `fidelity` | After each of 50 compactions, every pinned fact is an exact quote of its event and is carried over byte for byte | 100% (M2) |

## Running

```sh
cargo build --release -p harness-evals
B=target/release/harness-evals

# Live (uses your key from the keyring or environment, like harness):
$B needle   --model openai/gpt-6-luna
$B needle   --model openai/gpt-6-luna --baseline
$B fidelity --model openai/gpt-6-luna

# More seeds for a steadier number:
for s in 1 2 3; do
  $B needle --model openai/gpt-6-luna --seed $s
  $B needle --model openai/gpt-6-luna --seed $s --baseline
done

# Offline (no key, no cost): a deterministic model that answers only from
# what it is shown. It checks the harness logic, not a model's judgement.
$B needle --offline
$B needle --offline --baseline
$B fidelity --offline
```

Every run appends a row to `evals/results.tsv`: date, eval, variant, model,
seed, turns, compactions, items, correct, score, cost in US dollars, notes.

Options: `--turns` (200), `--needles` (10), `--compactions` (50), `--seed` (1),
`--compact-at` (6,000 input tokens: small, so a 200-turn run compacts several
times), `--effort` (low).

## What it costs (gpt-6-luna, standard prices of 18 Nov 2026)

| Run | Model calls | Input tokens (about) | Cost (about) |
|---|---|---|---|
| `needle`, 200 turns | ~205 | ~0.8M, much of it cached | under $0.10 |
| `needle --baseline` | ~205 | ~0.8M | under $0.10 |
| `fidelity`, 50 compactions | ~100 | ~0.2M | under $0.03 |

Three seeds of both needle variants plus one fidelity run stay under $1. The
`cost_usd` column records what each run actually cost, from the pricing table.

## Reading the results

- **needle:** `correct / items` is the accuracy. A wrong answer can come from
  the summary (the fact was not pinned), from the model (it did not use the
  pin or recall it), or from a failed turn (see `notes`).
- **fidelity:** `correct / items` counts pin checks, two per active pin per
  compaction: carried over unchanged, and present verbatim in the next opener.
  `notes` also says how many of the 50 requirements the model chose to pin. M2
  measures that what is pinned stays exact, not how much the model pins.
- `notes` lists failed turns and any broken invariant (I4–I6) found in the log.
