# harness-ports

Port traits (SDD layer L4). The runtime depends only on these; adapters implement them.

## Ports

| Trait | Purpose | Implemented in |
|---|---|---|
| `Provider` | One streamed model call (`stream`, `capabilities`, `count_tokens`) | `provider-*` (Phases 1, 12), `testkit::FakeProvider` |
| `Tool` | Something the model can call (`spec`, `risk`, `run`) | `tools`, `mcp` |
| `EventLog` | Append-only source of truth (`append`, `range`, `search`) | `store-sqlite` (Phase 3) |
| `BlobStore` | Content-addressed bytes | `store-sqlite` (Phase 3) |
| `Sandbox` | Rewrites a `CommandSpec` to run sandboxed | `sandbox` (Phase 13) |
| `ReadLedger` | Read-before-edit guard records | `tools` (Phase 2) |
| `EventSink` | Non-blocking live events | `runtime` |
| `Clock` | Injectable time; `SystemClock` provided | `testkit::FixedClock` for tests |

Ports for policy, hooks and memory are added in Phases 2, 7 and 14.

## Supporting types

- Provider: `ModelRequest`, `SystemPart`/`SystemSlot` (slot order 1–6 from Tech
  spec 2.6), `CachePlan`, `Effort`, `Capabilities`, `CacheStyle`, `ProviderEvent`,
  `ProviderError` (`is_retryable`, `retry_after`).
- Tools: `ToolName` (validated `[A-Za-z0-9_-]{1,64}`), `ToolSpec`, `ToolCtx`,
  `RiskCtx`, `ToolOutput`, `ToolError`.
- Storage: `AppendRequest`, `LoggedEvent`, `SearchHit`, `BlobMeta`, `StoreError`.
- Ledger: `FileStamp`, `LedgerCheck::compare` (content hash decides, mtime alone does not).

## Design notes

- Every trait is dyn-compatible; a unit test (`every_port_is_dyn_compatible`)
  keeps it that way, because the runtime holds adapters as `Arc<dyn …>`.
- `ProviderError::is_retryable` encodes FR-PRV-04: retry rate limits, overload,
  5xx, network errors and timeouts; never auth or invalid requests.
