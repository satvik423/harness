# harness-types

Shared domain types. Depends on no other harness crate.

## Public API

| Item | Purpose |
|---|---|
| `SessionId`, `TaskId`, `ApprovalId`, `QuestionId` | ULID newtypes; time-ordered, `Display`/`FromStr`, serialised as strings |
| `Seq` | Per-session, gapless event number; `Seq::FIRST == Seq(1)`, displayed as `#812` |
| `BlobRef` | 32-byte BLAKE3 content address; serialised as 64 lowercase hex characters |
| `ModelRef` | `provider/model`, for example `anthropic/claude-opus-5-5` |
| `Effort` | Reasoning effort, `low` … `max` (moved here in Phase 5 so the protocol can name it) |
| `Role`, `ContentBlock`, `Message` | Provider-neutral conversation content |
| `ContentBlock::Opaque` | Provider data that must round-trip byte for byte (signed thinking, encrypted reasoning) |
| `Usage`, `StopReason` | Token accounting and why a reply ended |
| `Risk` | What a tool call would touch; input to policy |
| `Mode` | The session's permission mode: `read-only`, `auto`, `plan`, `full`; `next()` for Shift+Tab, `describe()`, `FromStr`/`Display` (ADR-042) |

## Example

```rust
use harness_types::{ContentBlock, Message, ModelRef, Role};

let model: ModelRef = "anthropic/claude-opus-5-5".parse()?;
let reply = Message::new(Role::Assistant, vec![ContentBlock::text("Done.")]);
assert_eq!(model.provider(), "anthropic");
assert_eq!(reply.text(), "Done.");
# Ok::<(), harness_types::ParseModelRefError>(())
```

## Design notes

- Newtypes over bare strings and integers make illegal states unrepresentable
  (`ModelRef` cannot be built without a valid provider; `BlobRef` is always 32 bytes).
- `Message::tool_call_ids` / `tool_result_ids` support the I2 invariant check
  (every tool call answered before the next model call), which arrives in Phase 1.
- Arithmetic on `Usage` saturates instead of overflowing.
