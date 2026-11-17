# ADR-029: The policy port and rule semantics

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Phase 2 (P2-T5, P2-T6); details Tech spec 3.6

## Context

ADR-017 and the SDD add a `Policy` port in Phase 2 without defining it. Tech spec 3.6
gives the rules format and the evaluation order, but leaves several details open: what
a remembered approval allows, how `path` patterns combine with several paths, and how a
headless front-end learns that a denial was really "approval needed".

## Decision

- **Port.** `Policy::evaluate(&PolicyCall) -> Verdict` and
  `Policy::remember(&PolicyCall, GrantScope) -> Result<String, PolicyError>`.
  `Verdict` is `Allow { reason }`, `Ask { reason }` or `Deny { reason, approval_required }`.
  `approval_required` is set when ask became deny under approval policy `never`, so
  `harness run` can exit with code 4.
- **Every call is evaluated**, read-only calls included (a deny rule on `**/.env*` must
  stop `read_file`).
- **Built-in deny paths** (`policy.default_deny_paths`) apply to every tool. For `shell`
  they are checked against argument words and redirection targets, as a convenience on
  top of the sandbox (guardrail 11).
- **A `path` pattern** matches when any of the call's paths matches it, except in an
  `allow` rule, which must match every path. Allowing one file never allows a call that
  also touches another.
- **Remembered shell approvals match exactly.** "Session" and "always" store
  `match = "regex:^<command>$"` per simple command, so approving `rm -rf build` does not
  allow `rm -rf build ~`. Commands with substitutions or run-time values cannot be
  remembered; they are approved once and a warning says so.
- **Grants never override `ask` rules**, because all layers combine as deny > ask > allow.
- **"Always"** writes to `~/.config/harness/rules.toml` (user) or
  `<project>/.harness/rules.toml` (project; only when the project is trusted).
- **`match` subjects:** each simple command for `shell`, the tool's own name for MCP
  tools (`mcp__server__NAME`), the host for network calls; no subject otherwise.

## Consequences

The runtime stays free of rule logic: it asks the port, prompts on `Ask`, and calls
`remember` (on a blocking thread, since it may write a file) for session and permanent grants.
