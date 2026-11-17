# harness-testkit

Test support. Dev-dependency only; production crates must not depend on it
(enforced by the layering rules).

## Public API

| Item | Purpose |
|---|---|
| `FakeProvider` | Scripted `Provider`: `with_text_turn`, `with_tool_call_turn`, `with_tool_calls_turn`, `with_turn`; emits `BlockComplete` like a real provider; records every `ModelRequest` (`requests()`); honours cancellation; deterministic `count_tokens` (chars ÷ 4, rounded up) |
| `TempProject` | Temp project dir with `with_file` (rejects paths escaping the project) and `harness_dir` |
| `FixedClock` | `Clock` you can `set_ms` / `advance_ms` |
| `RecordingSink` | `EventSink` that keeps every event |
| `invariants::check_all` | Append-only (I1), tool pairing (I2) and no-prefill checks over recorded requests |
| `arch::DependencyGraph` | Workspace dependency graph from `cargo metadata` (normal deps only) |
| `arch::harness_rules`, `arch::adapter`, `arch::check` | The layering rules and the checker |

## Example

```rust
use harness_testkit::{FakeProvider, TempProject};

let project = TempProject::new()?.with_file("src/lib.rs", "pub fn f() {}")?;
let provider = FakeProvider::new()
    .with_tool_call_turn("c1", "read_file", &serde_json::json!({"path": "src/lib.rs"}))
    .with_text_turn("It defines f.");
// … run the code under test, then:
// assert_eq!(provider.requests().len(), 2);
```

## Design notes

- Tool-call input is split across two `ToolCallInputDelta` events so consumers
  must accumulate JSON fragments, as with real providers.
- `requests()` is what the append-only invariant tests (from Phase 1) inspect.
