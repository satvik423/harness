# Architecture decision records

Each record states the context, the decision and its consequences. Records 0001–0012 come from *Building Harness*, Part 5.2.

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
