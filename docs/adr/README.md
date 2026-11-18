# Architecture decision records

Each record states the context, the decision and its consequences. Records 0001–0012 come from *Building Harness*, Part 5.2. Records 0013 onward were made during the build and approved by the user.

| ADR | Title | Status |
|---|---|---|
| [ADR-001](0001-rust-workspace-ports-and-adapters.md) | Rust workspace with ports and adapters | Accepted |
| [ADR-002](0002-event-sourcing-on-sqlite.md) | Event sourcing on SQLite (WAL) as the single source of truth | Accepted |
| [ADR-003](0003-append-only-transcripts.md) | Append-only transcripts with segments | Accepted |
| [ADR-004](0004-anchored-compaction.md) | Anchored simple compaction instead of keep-tail or server-side compaction | Accepted |
| [ADR-005](0005-sqlite-memory-first.md) | SQLite FTS5 (then sqlite-vec) for memory; RushDB and graphify optional | Accepted |
| [ADR-006](0006-persona-bundle-subagent-instance.md) | Persona is a bundle; a subagent is a persona instance | Accepted |
| [ADR-007](0007-standards-over-invention.md) | Use AGENTS.md, SKILL.md and MCP as they are | Accepted |
| [ADR-008](0008-legitimate-auth-only.md) | Legitimate authentication only | Accepted |
| [ADR-009](0009-out-of-process-extensions.md) | Extensions run out of process | Accepted |
| [ADR-010](0010-linux-first-sandbox.md) | Linux-first sandbox via bubblewrap | Accepted |
| [ADR-011](0011-two-edit-tools.md) | Two edit tools | Accepted |
| [ADR-012](0012-raw-http-providers.md) | Raw HTTP providers | Accepted |
| [ADR-013](0013-crate-package-prefix.md) | Crate package names use the `harness-` prefix | Accepted |
| [ADR-014](0014-trust-list-file.md) | The trusted-project list is a TOML file | Accepted |
| [ADR-015](0015-protocol-event-additions.md) | Protocol events completed from the spec's flows | Accepted |
| [ADR-016](0016-runtime-may-use-context.md) | `runtime` may depend on `context` | Accepted |
| [ADR-017](0017-phase-0-conventions.md) | Phase 0 conventions | Accepted |
| [ADR-018](0018-licence.md) | Licence: MIT OR Apache-2.0 | Accepted |
| [ADR-019](0019-openai-provider-in-phase-1.md) | OpenAI provider moved into Phase 1 | Accepted |
| [ADR-020](0020-provider-event-block-complete.md) | `ProviderEvent::BlockComplete` | Accepted |
| [ADR-021](0021-tool-ctx-call-id.md) | `ToolCtx` carries `call_id` | Accepted |
| [ADR-022](0022-tool-registry-in-ports.md) | `ToolRegistry` lives in ports | Accepted |
| [ADR-023](0023-keyring-core.md) | Keyring access through `keyring-core` | Accepted |
| [ADR-024](0024-consecutive-user-messages.md) | A user message may follow an unanswered user message | Accepted |
| [ADR-025](0025-pause-turn.md) | `pause_turn` ends the task until server-side tools exist | Accepted |
| [ADR-026](0026-no-delegate-backends.md) | No delegate backends | Accepted |
| [ADR-027](0027-pty-process.md) | PTY tools on `pty-process` (user-approved change from the spec) | Accepted |
| [ADR-028](0028-ask-user-protocol.md) | `ask_user` events and the `UserChannel` port | Accepted |
| [ADR-029](0029-policy-port-and-rule-semantics.md) | The policy port and rule semantics | Accepted |
| [ADR-030](0030-event-sink-emit-with.md) | `EventSink::emit_with` for self-referencing events | Superseded by ADR-033 |
| [ADR-031](0031-task-control-and-session-locks.md) | Task control between processes, and session locks (user decision) | Accepted |
| [ADR-032](0032-segment-frame.md) | A segment's frozen prefix is stored as a frame | Accepted |
| [ADR-033](0033-session-writer-numbers-events.md) | The session's writer numbers events; the journal replaces `emit_with` | Accepted |
| [ADR-034](0034-snapshots-and-undo.md) | Snapshot ports and the undo op | Accepted |
| [ADR-035](0035-fork-in-phase-5.md) | Fork moves to Phase 5 (user decision) | Accepted |
| [ADR-036](0036-paused-tasks-and-recovery-edges.md) | Paused tasks, new input, and recovery edge cases | Accepted |
| [ADR-037](0037-ratatui-textarea.md) | The input box uses `ratatui-textarea` (user decision) | Accepted |
| [ADR-038](0038-model-and-effort-start-a-new-session.md) | `/model` and `/effort` apply to a new session (user decision) | Accepted |
| [ADR-039](0039-attachments-as-reference-notes.md) | Attached files become reference notes until Phase 11 (user decision) | Accepted |
| [ADR-040](0040-costs-debug-bodies-and-session-changes.md) | Costs in the runtime, debug bodies rebuilt from the log, session changes from snapshots | Accepted |
| [ADR-041](0041-deleting-sessions.md) | Deleting sessions, and empty sessions left by the TUI | Accepted |
| [ADR-042](0042-switchable-permission-modes.md) | Permission modes switch while a session runs; plan mode and `propose_plan` (user decision) | Accepted |
| [ADR-043](0043-approval-descriptions.md) | Approvals show a one-line description written by the model, next to the exact command | Accepted |
