# Architecture overview

This page tracks the *current* state of the code. The target architecture is in
Building Harness, SDD 2.3–2.4.

## Layers

```
L1 front-ends   harness-cli (binary: harness run)        [tui: Phase 4, server: Phase 15]
L2 protocol     harness-protocol   Op in → Event out, wrapped in Envelope{seq, ts, session}
L3 runtime      harness-runtime (session actor, turn engine), harness-context (requests, transcript rules)
L4 ports        harness-ports      Provider, Tool, ToolRegistry, EventLog, BlobStore, Sandbox, ReadLedger, EventSink, Clock
L5 adapters     harness-provider-anthropic, harness-provider-openai, harness-tools   [store-sqlite: Phase 3, …]

shared          harness-types (ids, messages, usage, risk), harness-config (layered config)
dev only        harness-testkit (FakeProvider, TempProject, FixedClock, layering checks)
```

## Dependency rules

Normal (non-dev) dependencies between workspace crates must follow these rules.
They are enforced by `harness_testkit::arch::harness_rules()` and the
`workspace_respects_layering_rules` integration test, which reads the real
`cargo metadata`. Every workspace crate must have a rule, so adding a crate
forces a decision about its layer.

| Crate | May depend on (workspace crates) |
|---|---|
| `harness-types` | nothing |
| `harness-protocol` | types |
| `harness-ports` | types, protocol |
| `harness-config` | types |
| `harness-context` | types, protocol, ports |
| `harness-runtime` | types, protocol, ports, context |
| `harness-tools`, `harness-provider-anthropic`, `harness-provider-openai` | anything except runtime, context, front-ends, testkit (`arch::adapter`) |
| `harness-testkit` | types, protocol, ports |
| `harness-cli` | anything except testkit |
| adapters *(later)* | anything except runtime, context, front-ends, testkit (`arch::adapter`) |

## Data flow (Phase 1)

1. `harness run` picks the provider from the model (`--model` or `default_model`),
   builds `RuntimeDeps` (provider, built-in tools, in-memory stores) and calls
   `spawn_session`.
2. It sends `Op::Submit`. The session actor opens segment 0 (environment block,
   AGENTS.md chain) and starts a task on its own tokio task.
3. The turn engine streams replies through `dyn Provider`, runs tool calls
   through `dyn Tool` (asking approval via `ApprovalRequested` / `Op::Approve`),
   and appends every message through `SegmentContext`, which enforces I1 and I2.
4. Every event is broadcast as an `Envelope`; the CLI renders it.
5. From Phase 3, persisted events are also written to the `EventLog` first.

## Cross-cutting rules already in code

- `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` in every crate.
- Workspace lints deny `unwrap`, `print!`/`eprint!`, `dbg!`, `todo!` and `unimplemented!` outside tests.
- Credentials never appear in config: secret fields are `keyring:` / `env:` references.
- Logs go to a daily-rotated file, never to the terminal.
