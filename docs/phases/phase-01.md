# Phase 01: First loop

## Summary

harness now runs a real agent end to end. `harness run PROMPT` sends the prompt to a
model, streams the reply, runs tool calls (`read_file` freely, `shell` after a y/N
approval), sends the results back, and repeats until the model is done, with
exit codes for scripts. The core is provider-neutral. Two adapters exist:
**OpenAI** (Responses API, verified live with `gpt-6-luna`) and **Anthropic**
(Messages API, verified against a mock server; no Anthropic API key was available).
OpenAI was moved forward from Phase 12 with your approval (ADR-019).

Every request the runtime sends is checked for the transcript invariants: append-only
(I1), every tool call answered in the next message (I2) and no assistant prefill.
They are enforced in code by `SegmentContext`, which refuses invalid appends, and in
tests over every recorded request.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P1-T1 | Done | `provider-anthropic`: request structs, `anthropic-version: 2026-10-21`, SSE via `eventsource-stream`, text and tool_use deltas |
| P1-T2 | Done | Adaptive thinking + effort; thinking blocks stored as `Opaque` and replayed verbatim; preserved-thinking binding control and refusal fallbacks sent |
| P1-T3 | Done | Retries with backoff + `retry-after` (shared `ports::backoff_delay`), typed errors, capabilities from `/v1/models/{id}` |
| P1-T4 | Partly done (see deviation 9) | OpenAI: 6 live recordings in `fixtures/streams/openai/`, 6 replay tests, insta snapshots of request bodies. Anthropic: recorder written, wiremock tests with protocol-accurate synthetic streams; live recordings wait for an Anthropic key |
| P1-T5 | Done | `tools`: `read_file`, `shell` (no sandbox, approval every time), `MemoryReadLedger`, `MemoryBlobStore`, `Unsandboxed`; `ToolRegistry` in ports |
| P1-T6 | Done | `runtime`: session actor, turn engine, approvals, interrupt/cancel, all results in one message |
| P1-T7 | Done | `context`: 2.6 slot order, built-in `default` persona, AGENTS.md chain, environment block in the opener |
| P1-T8 | Done | `harness run PROMPT [--json] [--model] [--max-turns] [--approval]`, exit codes 0/1/2/3/4/130, Ctrl-C interrupt |
| P1-T9 | Done | `testkit::invariants` (I1, I2, no prefill), run over every scripted session |
| (added) | Done | `provider-openai` (ADR-019), `default_model` config key, provider routing, keys from `keyring:openai` / `OPENAI_API_KEY` |

## Requirements covered

FR-PRV-01, FR-PRV-02 (moved from Phase 12), FR-PRV-03, FR-PRV-04, FR-TOOL-01,
FR-TOOL-02 (read_file, shell), FR-IN-01, FR-CTX-02, FR-UI-01; groundwork for
FR-TOOL-03 (read ledger) and NFR-12 (task/turn tracing spans).

## Exit criteria

| Criterion | Evidence |
|---|---|
| `harness run "how many .rs files are here and which is largest"` works against the real API | Run on this repo with `gpt-6-luna`, exit code 0. Two approved, read-only commands (`find … \| xargs wc -l \| sort`, `find … -printf`). Answer: "There are **92 `.rs` files**. The largest is `crates/runtime/tests/sessions.rs` at **825 lines** (25,790 bytes)." Checked independently: 92 files, 825 lines, 25,790 bytes |
| Invariant test passes on 5 scripted sessions | `runtime/tests/sessions.rs` `session_1` … `session_5` (text only; read-only tool; two calls in one turn; conversation across two tasks; denied + unknown + failing tools), plus 7 more sessions, all call `check_all` on every request: 0 violations |

All checks, run locally:

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 330 / 330 passed |
| `cargo test --workspace --doc` | 30 / 30 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 94.89% lines |

## Tests added and coverage

| Crate | Tests (total) | Line coverage |
|---|---|---|
| harness-runtime (new) | 25 (4 unit, 21 session) | **88.6%** (target 80%) |
| harness-context (new) | 23 | **97.2%** (target 80%) |
| harness-tools (new) | 26 | 95.7% |
| harness-provider-anthropic (new) | 42 (32 unit, 10 HTTP) | 94.7% |
| harness-provider-openai (new) | 37 (23 unit, 8 HTTP, 6 replay) | 95.3% |
| harness-cli | 36 (28 unit, 7 binary, 1 telemetry) | 92.4% |
| harness-testkit | 32 | 95.5% |
| harness-ports | 25 | 93.4% |
| harness-config / types / protocol | 45 / 32 / 7 | 97.4% / 97.4% / 100% |
| **Total** | **330** + 30 doctests | **94.89%** |

The `policy` crate starts in Phase 2.

## New dependencies

All are the latest stable releases on 17 Nov 2026, with no RustSec advisories, declared once in `[workspace.dependencies]`.

| Crate | Version | Why |
|---|---|---|
| reqwest | 0.13.5 (rustls, stream, json, http2, system-proxy) | HTTP client for both providers |
| eventsource-stream | 0.2.3 | SSE parsing (the plan forbids a hand-rolled parser) |
| tokio | 1.54.1 (now a normal dependency) | Runtime, processes, timers, signals |
| keyring-core | 1.0.1 | Keyring API (the `keyring` crate's docs recommend apps use this directly) |
| zbus-secret-service-keyring-store | 1.0.1 (rt-async-io-crypto-rust) | Linux Secret Service backend, pure-Rust crypto, no tokio coupling |
| nix | 0.32.0 (signal) | `killpg` to stop a whole command process group |
| blake3 | 1.8.7 | File stamps and blob addresses |
| wiremock | 0.6.5 (dev) | Mock HTTP server for provider tests |
| insta | 1.49.0 (dev, json) | Request body snapshots |

New allowed licences in `deny.toml`, all permissive and needed by the TLS stack:
ISC (rustls-webpki, untrusted, aws-lc), BSD-3-Clause (subtle),
CDLA-Permissive-2.0 (webpki root certificate data).

## Decisions / ADRs

- ADR-019: OpenAI provider moved into Phase 1; `default_model` config key; `ApiKey`
  and `backoff_delay` shared in ports.
- ADR-020 to ADR-025: deviations 1–6 below (BlockComplete, `call_id`, registry in ports,
  `keyring-core`, consecutive user messages, `pause_turn`).
- ADR-026: no delegate backends (your decision after the phase); `[providers.delegate]`
  removed from the config code.

## Deviations from the plan (approved by you on 17 Nov 2026; ADR-020 to ADR-025)

1. **`ProviderEvent` changed.** `ToolCallEnd` and `Opaque` were replaced by
   `BlockComplete { block }`, which carries each finished block exactly as the model
   produced it. The runtime builds the assistant message only from these. Rebuilding
   it from text deltas could merge two text blocks and break Claude's preserved
   thinking (ADR-003).
2. **`ToolCtx` gained `call_id`**, so tools can emit live `ToolOutput` events for their call.
3. **`ToolRegistry` lives in `ports`**, not the `tools` crate: runtime and context need
   it and must not depend on an adapter.
4. **`keyring-core` + `zbus-secret-service-keyring-store`** instead of the all-in-one
   `keyring` crate, as its own documentation advises for applications. Linux only for
   now; other platforms fall back to environment variables.
5. **Consecutive user messages are allowed** in `SegmentContext` when the previous turn
   produced no reply (refusal or empty). Without this, a refusal would block all
   further input. Both providers accept consecutive user turns.
6. **`pause_turn` ends the task with a warning.** It only happens with server-side
   tools, which harness does not use yet; continuing it properly would mean sending an
   assistant-final request. A test reproduced the bug before the fix.
7. **`shell` caps output at 30,000 characters (head + tail).** This is a stopgap until
   the general output capper in Phase 2.
8. **"Allow for session" and "always" act as one-off approvals** in Phase 1. Remembered
   approvals arrive with the policy engine in Phase 2.
9. **Anthropic fixtures are not recorded** (P1-T4): there is no Anthropic API key, and
   Anthropic's terms rule out using your subscription. The recorder is ready, and
   `fixtures/README.md` has the one-line command.

## Known issues

- With piped input (as in the exit check), the `✓` line appears on the same line as the
  approval prompt, because nothing echoes the answer. Interactive use is fine.
- Anthropic is unverified against the live API (see deviation 9).
- OpenAI `count_tokens` is a local estimate (characters ÷ 4); exact usage comes from each response.
- OpenAI capabilities are conservative defaults; the Models API reports no limits.
- `Usage.cost_micros` is always 0; cost accounting arrives in Phase 4.
- Every event, including streaming deltas, gets a sequence number from the runtime's counter.

## Notes for later phases

- **Phase 2:** remembered approvals (session/always) in the policy engine; replace the
  `shell` output cap with the general capper; parallel read-only tool calls; echo
  piped approval answers.
- **Phase 3:** the event log assigns sequence numbers to persisted events. Decide how
  deltas are numbered.
- **Phase 4:** cost per turn from the `[pricing]` table, shown in the TUI.
- **Phase 12:** no delegate backends (ADR-026); the phase keeps fallback chains and the
  Anthropic OAuth profile.
- **When an Anthropic key exists:** record the Anthropic fixtures and add
  `provider-anthropic/tests/replay.rs`; check OpenAI's exact token-count endpoint and
  whether `xhigh` effort is accepted (currently mapped to `high`).
- **Server-side tools (later):** continue `pause_turn` turns properly.
