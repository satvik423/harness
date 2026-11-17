# harness-tools

Built-in tools (Tech spec 3.5) and in-memory implementations of the support ports.
An adapter crate: it may depend on ports, types and protocol, never on the runtime.

## Tools (Phase 1)

| Tool | Input | Risk | Behaviour |
|---|---|---|---|
| `read_file` | `path`, `offset?` (1-based), `limit?` (≤ 2000) | ReadOnly | Numbered lines (`cat -n` style); long lines cut at 2,000 chars; binary files refused; every read recorded in the read ledger |
| `shell` | `command`, `timeout_s?` (1–600, default 120) | Exec | `/bin/sh -c` in the working directory, in its own process group; live output as `ToolOutput` events; output over 30,000 chars keeps head and tail; ends with `[exit code N]` |

`builtin_registry()` returns a `ToolRegistry` with both.

## Support implementations

| Type | Port | Notes |
|---|---|---|
| `MemoryReadLedger` | `ReadLedger` | Keys are canonical paths, so relative and absolute paths match. Kept long-term. |
| `MemoryBlobStore` | `BlobStore` | BLAKE3-addressed, deduplicated, in memory. Replaced by SQLite in Phase 3. |
| `Unsandboxed` | `Sandbox` | Passes commands through. Replaced by bubblewrap in Phase 13; until then every command needs approval. |

## Design notes

- **Process groups.** A timeout or interrupt kills the whole group (`killpg`), so
  `sleep 30 & sleep 30` cannot outlive the tool. After the shell exits, the tool
  waits 200 ms for its pipes to close; if a stray background job still holds
  them, the group is killed instead of hanging. Long-running servers get PTY
  tools in Phase 2.
- **UTF-8 across reads.** Incomplete multi-byte sequences are carried to the
  next read, so characters split by the pipe are not corrupted.
- **Input validation.** Inputs deserialize into private structs with
  `deny_unknown_fields`; schemas are marked `strict` for the provider.
