# Long sessions: compaction, pinned facts, recall and forks

A model can only read so much at once. When a session's context gets full, harness
**compacts** it into a new segment. Nothing is deleted: the full history stays in the
log, the important facts are kept word for word, and the model can fetch any earlier
detail exactly. (Design: SDD 2.6, Tech spec 3.13.)

## What a compaction does

1. The model writes a structured summary of the current segment: the goal, where the
   work stands, decisions, files touched, open questions and next steps.
2. It **pins** facts that must survive word for word: requirements, decisions,
   constraints and identifiers. harness checks every pin against the event it quotes
   and drops any that are not exact.
3. It divides the segment into **anchors**: event ranges with a topic and a list of
   what exact details they hold. harness fills any gaps, so the anchors cover every
   event.
4. A new segment starts from that summary, the pinned facts (all of them, from every
   compaction so far) and the anchor index. Nothing from the old segment is sent
   again, so stale thinking blocks never reach the model.

The transcript itself is never edited: compaction starts a new one.

## When it happens

- **Automatically**, when the next request would reach the threshold: the smaller of
  70% of the model's context window and `context.compact_at_tokens` (250,000 by
  default). harness prefers to compact between your messages. If a long task fills
  the context, it compacts after a tool round and continues with "Continue the task
  from the summary above." It never compacts while a tool call waits for its result.
- **When you ask:** `/compact` in the terminal UI, or `/compact the API design` to say
  what to keep in focus.
- **When you switch model or effort** with `/model` or `/effort`: the session continues
  in a new segment with the new settings.

In the terminal UI you see `◇ compacted segment 0 → 1 · 3 anchors · 4 pinned facts ·
182k → 6.1k tokens`. The header's segment number goes up and the context meter drops.

## Getting details back

- **The model** calls the `recall` tool. It can fetch an anchor
  (`{"anchor": "a2"}`), exact events (`{"seq_from": 812, "seq_to": 812}`, also used for
  the full output of a trimmed tool result) or matches of a search over earlier
  segments (`{"query": "tenant_id error"}`). Results come in pages of about 2,000 tokens.
- **You** can type `/recall` to list the anchors, then pick one. Or type `/recall a2`
  or `/recall tenant_id error`. The recalled events go with your next message (shown
  as ↺ above the input; Esc removes them).

## Forks

A fork is a new session that shares another session's history up to one event and
then goes its own way. Use it to try a different approach without losing the original.

- Terminal UI: `/fork` forks after the latest event; `/fork 42` forks after event #42.
- Command line: `harness session fork ID [--at SEQ]`. `harness session show ID` lists
  the event numbers.

Nothing is copied: the fork reads its parent's events up to the fork point. It starts
from a compaction of that history. A session that has forks cannot be deleted.

## Settings

```toml
[context]
compact_at_ratio = 0.7        # share of the model's window
compact_at_tokens = 250000    # absolute ceiling (cost matters long before 1M)
compaction_model = "same"     # or "openai/gpt-6-luna": a cheaper model on the same provider
```

`harness session show ID` lists `compacted`, `recalled` and `model_switched` events.
`harness debug diff-requests` compares only requests of the same segment.

## Evals

`evals/README.md` explains the needle and fidelity evals that measure all this (M1, M2).
