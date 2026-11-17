# Phase 02: Tools & policy

## Summary

harness is now a useful coding agent that asks before it acts. It can find, read,
edit, patch and create files, run commands and interactive programs, keep a
checklist and ask the user questions. Every tool call goes through a policy engine
(profiles, `rules.toml`, shell command analysis), and approvals can be once, for the
session, always (a saved rule) or denied with a reason. Tool output is capped once,
when recorded, with a footer pointing at the full output. Consecutive read-only
calls run in parallel.

All three exit criteria are met. The live run used `openai/gpt-6-luna`: it fixed the
seeded bug with **1 approval**. The patch fuzzer ran **10 minutes clean** (5.16 million
inputs), after an earlier run found a real round-trip bug, which is now fixed. A
deny rule on `**/.env*` stops `read_file`, `write_file` and `shell` alike.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P2-T1 | Done | `write_file`, `edit_file` on a shared guard (`fs_write`): existing files must be read in this session and unchanged; atomic writes (temp file + rename, permissions kept); the ledger is updated after a write so the model can keep editing; `edit_file` returns the changed region with line numbers |
| P2-T2 | Done | `glob` (gitignore-aware, newest first, ≤ 500) and `grep` (ripgrep libraries; `files` / `content` / `count`; context lines; glob filter; binary files skipped); both parallel-safe |
| P2-T3 | Done | `context::cap_output`: head 60% + tail 40% of the cap + `[output trimmed: N tokens total · full: recall {"seq_from":S,"seq_to":S}]`; applied once in the runtime when `ToolFinished` is recorded; full output inline, or as a blob over 64 KB; per-tool `output_cap_tokens` override. `EventSink::emit_with` lets the footer name its own event (ADR-030). The shell's 30k-character cap became an 8 MiB memory bound |
| P2-T4 | Done | `apply_patch`: parser for the 3.5 grammar (plus `*** End of File`), pure planner on a `PatchFs` view (exact, then trailing-whitespace, then whitespace-insensitive matching; each hunk unique after the previous one; CRLF kept), atomic commit with rollback. 4 proptests (512 cases each) and a cargo-fuzz target |
| P2-T5 | Done | New `harness-policy` crate: `rules.toml` (`prefix:` whole words, `glob:`, `regex:`; `path` globs with `~/`), layers (built-in deny paths, user, trusted project, session), deny > ask > allow, profile defaults, tree-sitter-bash splitting with force-ask for substitutions, subshells, `eval`, control flow and outside writes. `Policy` port in `ports` (ADR-029) |
| P2-T6 | Done | Runtime judges every call with the policy; `ApprovalRequested` carries the policy's reason; once / session / always (user or project) / deny with reason; grants are remembered on a blocking thread. CLI prompt `[y]es once / [s]ession / [a]lways / [p]roject always / [n]o (n <why>)`, piped answers echoed |
| P2-T7 | Done | Consecutive parallel-safe, allowed read-only calls run in a `JoinSet` behind a `Semaphore` (`runtime.max_parallel_tools`, default 4); results keep call order; approvals stay sequential |
| P2-T8 | Done | `todo` (checklist; CLI renders it) and `ask_user` (`UserChannel` port, `QuestionAsked` / `Op::Answer`, "no user available" when headless; ADR-028). `wrap_untrusted` in `ports`, used for `read_file` and `grep` content; base prompt v2 explains it |
| P2-T9 | Done | `pty_start`, `pty_write`, `pty_read`, `pty_kill` on `pty-process` (ADR-027): programs are session leaders, so `killpg` stops everything they started; output buffered (1 MiB), escape codes stripped; at most 8 programs; killed when the manager is dropped |

## Requirements covered

| Requirement | How |
|---|---|
| FR-TOOL-02 | Built-ins complete: `write_file`, `edit_file`, `glob`, `grep` added to `read_file` and `shell` |
| FR-TOOL-03 | `edit_file` (and `write_file`) refuse files not read in this session or changed since |
| FR-TOOL-04 | Every output capped once when recorded, with a recall footer naming the event that holds the full output |
| FR-TOOL-05 | `apply_patch`, multi-file and all-or-nothing |
| FR-TOOL-06 | PTY sessions: `pty_start`, `pty_write`, `pty_read`, `pty_kill` |
| FR-TOOL-07 (part) | `todo` and `ask_user` |
| FR-TOOL-08 | Read-only calls in one reply run in parallel; all results go back in one message |
| FR-SAFE-01 | Allow, ask and deny rules on tool, command prefix and path; deny > ask > allow |
| FR-SAFE-02 | Approval prompts: once, this session, always (saved as a rule), deny with a reason |
| FR-SAFE-03 (part) | Approval policy (on-request, never) and profiles; sandbox modes arrive in Phase 13. The `untrusted` approval policy is not built yet (see notes) |
| FR-SAFE-06 | File content marked untrusted (`<untrusted source="…">`); web and MCP use the same wrapper when they arrive |

## Exit criteria

1. **Fixes a seeded bug in a fixture project with fewer than 3 approvals.**
   - Fixture: `fixtures/projects/seeded-bug/`, where `average()` divides by `count + 1`.
   - Scripted, deterministic: `cargo test -p harness-cli fixes_the_seeded_bug_with_fewer_than_three_approvals`
     → passes. Real tools and the real policy engine, 1 approval, and the fixture's tests pass afterwards.
   - Live, with `openai/gpt-6-luna` on a scratch copy:
     `printf 's\ns\ns\ns\n' | harness run --max-turns 20 "The tests in this project fail. Run them with: sh test.sh. Find the bug, fix it, and confirm the tests pass."`
     → exit 0. **1 approval** (`sh test.sh`, answered "session", so the re-run did not ask).
     The model used `todo`, `glob`, four parallel `read_file` calls, `shell`, `edit_file` and
     `shell` again, then answered "Fixed `average()` in `lib/stats.sh` to divide by the
     actual number of arguments…". Afterwards `sh test.sh` printed four `ok` lines.
2. **A deny rule on `**/.env*` is respected.**
   - `cargo test -p harness-cli the_default_deny_rule_on_env_files_is_respected` → passes:
     `read_file .env`, `write_file .env.local` and `shell cat .env` are all denied without
     a prompt, the secret never appears in any request, and `.env.local` is not created.
   - Policy unit tests cover `**/.env*` under nested folders, `../` tricks and shell arguments.
3. **Patch fuzzing runs 10 minutes clean.**
   - `cd fuzz && cargo +nightly fuzz run apply_patch corpus/apply_patch seeds/apply_patch -- -max_total_time=600`
     → `Done 5162626 runs in 601 second(s)`, no crashes, `artifacts/` empty.
   - The first run crashed after 205,599 inputs: a hunk line ending in `\r` lost one `\r`
     per render/parse round trip, because the parser stripped `\r` from every line, even in
     an LF patch. Fixed test-first (`a_carriage_return_in_an_lf_patch_is_line_content`); the
     parser now strips `\r` only from CRLF patches, and the proptest generator now includes `\r`.

Checks (all pass):

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 512 passed, 0 failed |
| `cargo test --workspace --doc` | 41 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 95.11% lines |

## Tests added and coverage

| Crate | Tests (total) | Line coverage |
|---|---|---|
| harness-runtime | 40 (6 unit, 34 session) | **91.0%** (target 80%) |
| harness-context | 31 | **97.6%** (target 80%) |
| harness-policy (new) | 41 | **92.9%** (target 80%) |
| harness-tools | 124 (120 unit, 4 property) | 96.0% |
| harness-cli | 49 (41 unit, 7 binary, 1 telemetry) | 93.6% |
| harness-ports | 31 | 95.3% |
| harness-testkit | 32 | 95.1% |
| harness-provider-anthropic / openai | 42 / 37 | 94.7% / 95.3% |
| harness-config / types / protocol | 46 / 32 / 7 | 97.4% / 97.4% / 100% |
| **Total** | **512** + 41 doctests (Phase 1: 330 + 30) | **95.11%** |

The append-only invariant checks (I1, I2, no prefill) run in every runtime session
test, including the new approval, parallel and `ask_user` sessions.

## New dependencies

All are the latest stable releases on 17 Nov 2026, with no RustSec advisories, declared once in `[workspace.dependencies]`.

| Crate | Version | Why |
|---|---|---|
| `ignore` | 0.4.34 | gitignore-aware directory walking for `glob` and `grep` |
| `globset` | 0.4.21 | glob matching for `glob`, rule `glob:` patterns and tool-name patterns |
| `grep-searcher` | 0.1.17 | ripgrep's search engine for `grep` (line numbers, context, binary detection) |
| `grep-regex` | 0.1.14 | ripgrep's regex matcher for `grep` |
| `regex` | 1.13.1 | rule `regex:` patterns; exact-match remembered commands |
| `tree-sitter` | 0.27.2 | parser runtime for shell command analysis |
| `tree-sitter-bash` | 0.25.1 | bash grammar: splits compound commands, finds substitutions |
| `pty-process` | 0.5.4 (`async`) | pseudo-terminals for the PTY tools (ADR-027) |
| `libfuzzer-sys` | 0.4.13 | the `apply_patch` fuzz target only (`fuzz/`, outside the workspace) |

Dev: `proptest` (already in the workspace) is now used by `tools` and `policy`;
`ports` gains `tokio` as a dev-dependency for async tests. `tree-sitter` brings in
`foldhash` (Zlib licence), so `Zlib` was added to `deny.toml`'s allowed licences;
it is permissive and OSI- and FSF-approved. Tooling (not dependencies):
nightly Rust and `cargo-fuzz` 0.13.2, for fuzzing only.

## Decisions / ADRs

- **ADR-027** PTY tools on `pty-process`, a change from the spec approved by the user
  when P2-T9 started. `pty_read` and `pty_kill` have read-only risk.
- **ADR-028** `ask_user` events (`QuestionAsked`, `QuestionAnswered`, `Op::Answer`),
  `QuestionId`, and the `UserChannel` port in `ToolCtx`.
- **ADR-029** The policy port (`evaluate`, `remember`, `Verdict::Deny { approval_required }`)
  and rule semantics. Every call is evaluated; built-in deny paths apply to every tool;
  `allow` + `path` must cover every path; remembered shell commands match exactly;
  grants never override `ask` rules.
- **ADR-030** `EventSink::emit_with`, so the capped result's footer can name its own event.
- Smaller choices:
  - "always" writes to `~/.config/harness/rules.toml`, or the project's `.harness/rules.toml` with `p`.
  - `apply_patch` rejects absolute paths, refuses to add an existing file or to rename onto one,
    and allows changing a file that was never read (its context lines prove what the model
    expects), but not one changed since it was read.
  - Base prompt v2 mentions the new tools and the `<untrusted>` wrapper.
  - The example targets were renamed to `record_anthropic_fixtures` and
    `record_openai_fixtures`, ending cargo's output-filename collision warning.

## Deviations from the plan (and user approval)

| Change | Approval |
|---|---|
| `pty-process` instead of `portable-pty` (ADR-027) | Approved by the user during the phase, when P2-T9 started |

Everything else fills gaps the spec leaves open (ADR-028, -029, -030) without
changing anything it states.

## Known issues

- Commands always ask under the `auto` profile, because there is no sandbox yet
  (Phase 13). Session grants and `allow` rules reduce the questions.
- The PTY tools and `shell` are Unix-only (ADR-027).
- One `PtyManager` is shared by the whole process, not owned per session. With one
  session per `harness run` this is the same thing; it matters once a process hosts
  several sessions (server, Phase 10).
- The output cap estimates tokens as characters ÷ 4.
- `glob`/`grep` skip hidden files, as ripgrep does by default.

## Notes for later phases

- **Phase 3:**
  - The event log must keep `emit_with`'s guarantee (number and send together).
  - The recall footer's `seq` then becomes a persisted sequence number.
  - Snapshot files before `write_file` / `edit_file` / `apply_patch` (`ToolOutput::touched` is already filled).
  - On crash recovery, never re-run `pty_write` (Exec).
- **Phase 5:** the `recall` tool resolves the footer's `{"seq_from","seq_to"}`, reading the full output from the inline text or the blob.
- **Phase 6:** persona profiles become a policy layer (spec 3.6 step 3); `PolicySettings` is the place to add it.
- **Phase 7:** PreToolUse hooks run before `Policy::evaluate` (spec 3.6 step 1).
- **Phase 9 (TUI):** render `QuestionAsked` with selectable options, and `todo` as a live checklist.
- **Phase 13:**
  - Set `PolicySettings::sandboxed` from the real sandbox mode, so commands stop asking under `auto`.
  - Add the escalation flow (`escalate`, `justification` on `shell`).
- **MCP / web phases:** wrap their output with `wrap_untrusted`.
- **FR-SAFE-03:** the `untrusted` approval policy (ask for everything not known to be
  safe) is not in the 3.6 profile table; add it with the sandbox modes in Phase 13.
