# harness-tools

Built-in tools (Tech spec 3.5) and in-memory implementations of the support ports.
An adapter crate: it may depend on ports, types and protocol, never on the runtime.

## Tools

| Tool | Input | Risk | Behaviour |
|---|---|---|---|
| `read_file` | `path`, `offset?` (1-based), `limit?` (≤ 2000) | ReadOnly | Numbered lines (`cat -n` style), wrapped in `<untrusted source="file:…">`; long lines cut at 2,000 chars; binary files refused; every read recorded in the read ledger |
| `write_file` | `path`, `content` | Write | Creates a file, or replaces one that was read in this session and is unchanged since; atomic; identical content is not rewritten; eager input streaming |
| `edit_file` | `path`, `old`, `new`, `replace_all?` | Write | `old` must match exactly once (or `replace_all`); refused if the file was not read or changed since; returns the changed region with line numbers |
| `apply_patch` | `patch` (Codex format) | Write (every path, rename targets included) | Add, delete, update and rename files all-or-nothing; hunks found exactly, then ignoring trailing whitespace, then ignoring all surrounding whitespace; refuses files changed since they were read |
| `glob` | `pattern`, `path?` | ReadOnly, parallel-safe | gitignore-aware; `*` stays in one directory, `**/` recurses; newest first; at most 500 |
| `grep` | `pattern`, `path?`, `glob?`, `context?` (≤ 10), `mode?` (`files` default, `content`, `count`), `ignore_case?` | ReadOnly, parallel-safe | ripgrep libraries; binary files skipped; `content` output is `path:line:text` with `--` between groups, wrapped as untrusted; at most 2,000 lines |
| `shell` | `command`, `timeout_s?` (1–600, default 120) | Exec | `/bin/sh -c` in its own process group; live output as `ToolOutput` events; output kept in bounded memory (8 MiB); ends with `[exit code N]` |
| `pty_start` | `command`, `wait_ms?` | Exec | Starts a program on its own pseudo-terminal (session leader); returns id `ptyN` and first output |
| `pty_write` | `id`, `input`, `wait_ms?` | Exec | Types into the program (`\n` is Enter, `\u0003` Ctrl-C); returns new output |
| `pty_read` | `id`, `wait_ms?` (≤ 30,000) | ReadOnly (ADR-027) | Output since the last read; escape codes and `\r` removed; reports exit |
| `pty_kill` | `id` | ReadOnly (ADR-027) | Kills the program's whole process group |
| `todo` | `items[{text, status}]` | ReadOnly | Replaces the checklist; at most one item `in_progress`; the UI renders it from the call |
| `ask_user` | `question`, `options?`, `multi?` | ReadOnly | Asks through `ToolCtx::user` (ADR-028); headless runs get "no user available" |

`builtin_registry()` returns a `ToolRegistry` with all of them. The PTY tools share
one `PtyManager` (at most 8 programs; 1 MiB of unread output each).

## Modules

- `fs_write`: the shared read-before-edit guard (`check_fresh`), the snapshot
  before each write (`ToolCtx::snapshots`, for undo; no snapshot, no write), atomic writes
  (temp file in the same directory, then rename, keeping permissions) and the
  ledger update after a write, so the model can keep editing a file it wrote.
- `patch` (public): `parse_patch` → `Patch`, `plan(&Patch, &dyn PatchFs)` → the
  final state of every file, computed in memory. The tool then stages every new
  file as a temp file and renames them into place; if a rename fails, files already
  changed are restored. The parser and planner are property-tested and fuzzed
  (`fuzz/`, `cargo +nightly fuzz run apply_patch`).

## Support implementations

| Type | Port | Notes |
|---|---|---|
| `MemoryReadLedger` | `ReadLedger` | Keys are canonical paths, so relative and absolute paths match. Kept long-term. |
| `MemoryBlobStore` | `BlobStore` | BLAKE3-addressed, deduplicated, in memory. Replaced by SQLite in Phase 3. |
| `Unsandboxed` | `Sandbox` | Passes commands through. Replaced by bubblewrap in Phase 13; until then the policy asks before commands. |

## Design notes

- **Output capping is not done here.** Tools return their whole output; the runtime
  caps it once, when it is recorded (FR-TOOL-04). `shell` only bounds memory.
- **Process groups.** A timeout or interrupt kills the whole group (`killpg`), so
  `sleep 30 & sleep 30` cannot outlive the tool. PTY programs are session leaders,
  so their group is their pid.
- **UTF-8 across reads.** Incomplete multi-byte sequences are carried to the next
  read in `shell` and the PTY tools.
- **Untrusted content.** File contents are wrapped with `harness_ports::wrap_untrusted`
  (guardrail 12); the base prompt explains the wrapper.
- **Input validation.** Inputs deserialize into private structs with
  `deny_unknown_fields`.
