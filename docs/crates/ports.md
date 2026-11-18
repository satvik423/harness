# harness-ports

Port traits (SDD layer L4). The runtime depends only on these; adapters implement them.

## Ports

| Trait | Purpose | Implemented in |
|---|---|---|
| `Provider` | One streamed model call (`stream`, `capabilities`, `count_tokens`) | `provider-anthropic` (Phase 1), `provider-openai` (Phase 12), `testkit::FakeProvider` |
| `Tool` | Something the model can call (`spec`, `risk`, `run`) | `tools`, `mcp` |
| `EventLog` | Append-only source of truth (`append` a numbered `LoggedEvent`, `last_seq`, `range`, `search`); gapless numbers (ADR-033) | `store-sqlite`, `testkit::MemoryStore` |
| `BlobStore` | Content-addressed bytes | `store-sqlite`, `testkit::MemoryStore` |
| `SessionStore` | Sessions, tasks (kept from `TaskState` events), task control requests (ADR-031), `delete_session` (ADR-041) | `store-sqlite`, `testkit::MemoryStore` |
| `SnapshotStore`, `FileSnapshots` | File snapshots before writes, undo, and each file's first snapshot for `/diff` (ADR-034, ADR-040); `NoSnapshots` outside a session | `store-sqlite`, `runtime` |
| `Sandbox` | Rewrites a `CommandSpec` to run sandboxed | `sandbox` (Phase 13) |
| `ReadLedger` | Read-before-edit guard records | `tools` (Phase 2) |
| `EventSink` | Non-blocking live (non-persisted) events from tools | `runtime`, `testkit::RecordingSink` |
| `Clock` | Injectable time; `SystemClock` provided | `testkit::FixedClock` for tests |
| `Policy` | `evaluate` a call → `Verdict` (allow, ask, deny); `remember` approvals (ADR-029) | `policy`, `testkit::AskForChanges` |
| `UserChannel` | A tool asks the user a question (`ask_user`); `NoUser` for headless (ADR-028); `NO_ANSWER` is what a closed input answers | `runtime` |
| `History` | What `recall` reads: earlier events by anchor, range or search, rendered (`NoHistory` outside a session) | `runtime` |
| `ModeControl` | Read and switch the session's permission mode (`propose_plan` uses it); `FixedMode` outside a session (ADR-042) | `runtime` |

Ports for hooks and memory are added in Phases 7 and 14.

## Supporting types

- Provider: `Effort` (re-exported from `harness-types`), `ModelRequest`, `SystemPart`/`SystemSlot` (slot order 1–6 from Tech
  spec 2.6), `CachePlan`, `Effort`, `Capabilities`, `CacheStyle`, `ProviderEvent`,
  `ProviderError` (`is_retryable`, `retry_after`).
- Tools: `ToolName` (validated `[A-Za-z0-9_-]{1,64}`), `ToolSpec`, `ToolCtx`
  (including the `call_id` of the call being run, `user` for `ask_user`, `snapshots` for undo, `mode` for the permission mode and `history` for `recall`), `RiskCtx`, `ToolOutput`,
  `ToolError`, and `ToolRegistry` (tools by name; specs sorted by name).
- Streaming: `ProviderEvent::BlockComplete` carries each finished content block
  exactly as the model produced it. The runtime builds the transcript only from
  these; deltas and `ToolCallStart`/`ToolCallInputDelta` are for live display.
- Storage: `LoggedEvent`, `SearchHit`, `BlobMeta`, `SessionRecord`, `SessionStatus`, `TaskRecord`, `TaskControl`, `Snapshot`, `Undone`, `StoreError` (incl. `Conflict`).
- Ledger: `FileStamp`, `LedgerCheck::compare` (content hash decides, mtime alone does not).
- Policy: `PolicyCall` (with the `mode` in force), `Verdict`, `GrantScope` (session, user, project), `PolicyError`.
- `wrap_untrusted(source, text)`: wraps file, web and MCP content in
  `<untrusted source="…">` and defuses any tag inside it (guardrail 12).

## Design notes

- Every trait is dyn-compatible; a unit test (`every_port_is_dyn_compatible`)
  keeps it that way, because the runtime holds adapters as `Arc<dyn …>`.
- `ProviderError::is_retryable` encodes FR-PRV-04: retry rate limits, overload,
  5xx, network errors and timeouts; never auth or invalid requests.
