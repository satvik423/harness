# harness-protocol

The only interface between front-ends and the core: `Op` goes in, `Event` comes out.

## Public API

| Item | Purpose |
|---|---|
| `Op` | Requests into the core: create/resume session, submit input, interrupt, pause, resume, cancel, approve, answer (`ask_user`), undo, set the permission mode (`set_mode`, ADR-042), switch model or effort (`switch_model`, ADR-046), compact, switch persona, fork, shutdown. Tagged `{"op": "…"}` |
| `Decision`, `RuleScope` | Answers to approval requests (once, for session, always, deny) |
| `InputItem` | Text, a file path, or an existing blob |
| `Event` | Everything the core reports, including `question_asked` / `question_answered` for `ask_user` (ADR-028); `segment_opened` carries the frame blob (ADR-032); `session_opened` records the starting mode and `mode_changed` each switch, with the note for the model (ADR-042); `approval_requested` has an optional one-line `summary` from the model (ADR-043); `compacted` carries the validated `CompactionRecord` (ADR-045); `model_switched` records a model or effort switch (ADR-046). Tagged `{"type": "…"}` |
| `Event::kind` | Stable snake_case name, equal to the serialised tag; stored in `events.kind` |
| `Event::is_persisted` | `false` only for streaming deltas (`text_delta`, `thinking_delta`, `tool_output`) |
| `Envelope` | `{seq, ts, session, event}`: an event with its position in the log |
| `TaskState` | The task state machine's states; `is_terminal()` for completed, failed and cancelled |
| `HookOutcome`, `BlobOrText` | Supporting types for hook and tool events |
| `CompactionRecord`, `PinnedFact`, `Anchor`, `KeyDecision`, `PinKind` | What a compaction produced (Phase 5) |

## Example

```rust
use harness_protocol::{Event, Op};
use harness_types::TaskId;

let op = Op::Pause { task: TaskId::new() };
assert!(serde_json::to_string(&op)?.contains("\"op\":\"pause\""));
assert!(!Event::TextDelta { task: TaskId::new(), text: "Hi".into() }.is_persisted());
# Ok::<(), serde_json::Error>(())
```

## Design notes

- The same `Event` values are broadcast and stored, so the log replays exactly
  what the UI saw. Final content is in `AssistantMessage` and `ToolFinished`;
  deltas are display-only.
- Events that the spec's flows rely on but Tech spec 3.3 did not list were added:
  `UserInput`, `SystemNote`, `AssistantAborted`, `ApprovalResolved`,
  `CompactionStarted`, `SegmentOpened`, `UndoApplied`. Without them a transcript
  could not be rebuilt from the log.
