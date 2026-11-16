# Phase 00: Foundations

## Summary

The workspace and its foundations are in place: six crates (`types`, `protocol`,
`ports`, `config`, `testkit`, `cli`), workspace lints, a pinned toolchain, a
`cargo deny` policy, GitHub Actions CI, a layering checker that fails the build
if the runtime ever depends on an adapter, and a full `docs/` tree with
ADR-001 to ADR-012. Everything was built test-first. 130 tests and 18 doctests
pass, total line coverage is 96.2%, and every check in CLAUDE.md passes locally.

Environment setup done at the start of the phase, with the user's approval:
rustup + Rust 1.100.0 stable (user-local, `~/.cargo`; the installer added
`. "$HOME/.cargo/env"` to `~/.bashrc` and `~/.profile`), plus
cargo-nextest 0.9.152, cargo-deny 0.21.0 and cargo-llvm-cov 0.9.1.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P0-T1 | Done | `git init`; `Cargo.toml` workspace (edition 2024, resolver 3), `rust-toolchain.toml` (1.100.0 + clippy, rustfmt, llvm-tools), workspace lints (`unsafe_code` forbid, `missing_docs` deny, clippy deny `unwrap_used`, `print_stdout`, `print_stderr`, `dbg_macro`, `todo`, `unimplemented`), `clippy.toml` (tests may unwrap/print), `rustfmt.toml`, `.gitignore` |
| P0-T2 | Done (not yet run on GitHub) | `.github/workflows/ci.yml`: fmt, clippy, nextest + doctests on `ubuntu-24.04` and `ubuntu-24.04-arm`, rustdoc with `-D warnings`, cargo-deny, llvm-cov. YAML validated locally; no remote exists yet, so it has not run |
| P0-T3 | Done | `harness-types` (ids, `BlobRef`, `ModelRef`, `Message`/`ContentBlock` incl. `Opaque`, `Usage`, `StopReason`, `Risk`) and `harness-protocol` (`Op`, `Event`, `Envelope`, `TaskState`, …) with serde round-trip tests for every variant |
| P0-T4 | Done | `harness-ports`: `Provider`, `Tool`, `EventLog`, `BlobStore` (spec 3.4, 3.5, 3.12) plus `Sandbox`, `ReadLedger`, `EventSink` and `Clock`, which `ToolCtx` needs. Dyn-compatibility test |
| P0-T5 | Done | `harness-config`: typed schema with spec 3.17 defaults, `deny_unknown_fields`, `validate()`, layered `ConfigLoader` (SDD 2.10 order), `HARNESS_SECTION__KEY` env layer, `SecretRef` (`keyring:` / `env:` only), `TrustStore` |
| P0-T6 | Done | `harness-cli`: `telemetry` module (daily-rotated file, 7 kept, `HARNESS_LOG` filter, default `info`); `harness` binary with `--help` / `--version` |
| P0-T7 | Done | `harness-testkit`: `FakeProvider` (scripted turns, records requests, honours cancellation), `TempProject`, `FixedClock` |
| P0-T8 | Done | `docs/README.md`, `architecture/overview.md`, six crate pages, `guides/configuration.md`, `adr/0001`–`0012` + index, this report |
| P0-T9 | Done | `harness_testkit::arch`: graph from `cargo metadata` (normal deps only), allowlist/denylist rules, "every crate needs a rule". Integration test `workspace_respects_layering_rules` |

## Requirements covered

- **NFR-07** (modularity): ports-and-adapters split with an enforced dependency direction.
- **NFR-11** (testability): `FakeProvider`, `FixedClock`, `TempProject`; coverage tooling in place.
- **NFR-08** (security defaults, partial): credentials can only be referenced, never stored, in config.
- **FR-SAFE-05** (project trust, foundation): untrusted project config is never read.
- Groundwork for **FR-PRV-03/04** (capabilities, retry classification), **FR-TOOL-03** (`LedgerCheck`) and **FR-IN-03** (`BlobRef`).

## Exit criteria

| Criterion | Evidence |
|---|---|
| `cargo build` and `test` pass | `cargo nextest run --workspace --all-features` → `130 tests run: 130 passed, 0 skipped`; `cargo test --workspace --doc` → 18 passed |
| Pass **in CI** | Not verifiable yet: there is no Git remote. The workflow runs exactly the commands above. See *Known issues* |
| Dependency test fails if runtime imports an adapter | `arch::tests::runtime_depending_on_an_adapter_is_a_violation` and `project_rules_cover_core_crates_and_forbid_adapter_imports_in_runtime` show `check` reports the edge. `tests/architecture.rs` runs the real rules against the real workspace and passes today |

All CLAUDE.md checks, run locally:

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 130 / 130 passed |
| `cargo test --workspace --doc` | 18 / 18 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok (1 warning, see below) |
| `cargo llvm-cov --workspace --summary-only` | 96.18% lines, 93.9% regions |

## Tests added and coverage

| Crate | Unit / integration tests | Doctests |
|---|---|---|
| harness-types | 32 (incl. 1 proptest) | 4 |
| harness-protocol | 7 | 1 |
| harness-ports | 17 | 3 |
| harness-config | 44 | 4 |
| harness-testkit | 21 + 1 integration | 4 |
| harness-cli | 4 + 4 integration (binary, telemetry) | 2 |
| **Total** | **130** | **18** |

Line coverage is 96.18% overall. Lowest files: `ports/src/tool.rs` 79% (the
`Debug` impl and trait default methods, exercised once tools exist),
`types/src/ids.rs` 88%, `testkit/src/project.rs` 88%. The 80% targets for
`runtime`, `context` and `policy` do not apply yet; those crates start in
Phases 1, 5 and 2.

## New dependencies

All are the latest stable releases as of 16 Nov 2026, have no RustSec
advisories (`cargo deny`), and are declared once in `[workspace.dependencies]`.

| Crate | Version | Why |
|---|---|---|
| serde | 1.0.229 | Serialisation of every type, protocol and config |
| serde_json | 1.0.154 | Tool inputs, opaque provider blocks, `cargo metadata` parsing |
| thiserror | 2.0.21 | Typed library errors |
| ulid | 3.0.0 (serde) | Time-ordered session, task and approval ids |
| async-trait | 0.1.92 | Dyn-compatible async port traits (`Arc<dyn Provider>`) |
| futures | 0.3.34 (std, alloc only) | `BoxStream` for provider streams |
| tokio-util | 0.7.20 (no default features) | `CancellationToken` in provider and tool ports |
| toml | 1.2.1 | Config files, trust file |
| directories | 6.0.0 | Platform config and data directories |
| clap | 4.6.7 (derive) | Command-line parsing |
| anyhow | 1.0.104 | Error reporting in the binary only |
| tracing | 0.1.44 | Structured logging |
| tracing-subscriber | 0.3.23 (env-filter, fmt, std; no ANSI) | `HARNESS_LOG` filtering and formatting |
| tracing-appender | 0.2.5 | Daily-rotated, non-blocking log file |
| tempfile | 3.27.0 | Temp projects (testkit) and test fixtures |
| proptest | 1.12.0 (dev) | Property tests for parsers |
| tokio | 1.54.1 (dev, macros + rt) | Async tests in testkit |

## Decisions / ADRs

- ADR-001 to ADR-012 recorded from Building Harness Part 5.2 (`docs/adr/`).
- No new ADRs. The items below need your approval first, as CLAUDE.md requires.

## Deviations from the plan (awaiting your approval)

1. **Package names use a `harness-` prefix** (`harness-types`, `harness-ports`, …). Directories are unchanged (`crates/types`). Generic names like `types` and `config` are confusing in `use` paths and docs, and collide with crates.io names.
2. **The trust list is `~/.local/share/harness/trust.toml`**, not a table in `user.db` as SDD 2.9 says. SQLite only arrives in Phase 3. The TOML file is human-readable and saved atomically.
3. **Seven `Event` variants were added** that the spec's flows (F1–F3, 3.13, 3.14) use but Tech spec 3.3 did not list: `UserInput`, `SystemNote`, `AssistantAborted`, `ApprovalResolved`, `CompactionStarted`, `SegmentOpened`, `UndoApplied`. Without them a transcript can't be rebuilt from the log. The spec's "ApprovalGranted" is named `ApprovalResolved`, because it also records denials.
4. **`runtime` may depend on `context`**, as SDD 2.4's crate map says. CLAUDE.md rule 1 says runtime depends only on `ports`, `protocol` and `types`. I followed the SDD. Please confirm which one wins.
5. **`ApprovalId` lives in `types`** with the other ids, and `Seq` is a newtype, not a type alias as in spec 3.3.
6. **Policy, HookRunner and MemoryStore ports are not created yet.** They will be defined in Phases 2, 7 and 14 when they are first used. `Sandbox`, `ReadLedger`, `EventSink` and `Clock` were added now because `ToolCtx` needs them.
7. **Environment variable scheme:** `HARNESS_SECTION__KEY` (double underscore = nesting). The spec only said `HARNESS_*`.
8. **Workspace members are listed explicitly** instead of the `crates/*` glob, because a crate folder without a manifest breaks the glob.
9. **Licence:** the manifests say `MIT OR Apache-2.0` (from the spec's Cargo excerpt), but there are no `LICENSE` files yet. Tell me which licence you want and I'll add the files.

## Known issues

- **CI has not run**: there is no Git remote yet. The aarch64 job is untested, and the `ubuntu-24.04-arm` runner is only free for public repositories.
- `cargo deny` warns about two versions of `syn` in the dependency tree (transitive, from proc-macro crates). This is a warning, not a failure.
- Running `harness` writes a log file to `~/.local/share/harness/logs` even for `--help`-style runs. This is intended.

## Notes for later phases

- **Phase 1:** add `harness-runtime` and `harness-provider-anthropic` with rules in `arch::harness_rules()` (use `arch::adapter(...)` for adapters). Add secret resolution (keyring and env) behind a small port. Turn on the append-only invariant test with `FakeProvider::requests()`.
- **Phase 1:** the provider must send `anthropic-version` from `config.providers.anthropic.api_version` (default `2026-10-21`).
- **Phase 2:** add the `Policy` port. `config.policy.default_deny_paths` is already loaded.
- **Phase 3:** decide whether the trust list moves into `user.db`, depending on deviation 2.
- **Phase 6:** add the `harness trust` command; `TrustStore` is ready.
