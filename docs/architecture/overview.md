# Architecture overview

This page tracks the *current* state of the code. The target architecture is in
Building Harness, SDD 2.3–2.4.

## Layers

```
L1 front-ends   harness-cli (binary)            [tui, server: later phases]
L2 protocol     harness-protocol   Op in → Event out, wrapped in Envelope{seq, ts, session}
L3 runtime      [runtime, context: Phase 1+]
L4 ports        harness-ports      Provider, Tool, EventLog, BlobStore, Sandbox, ReadLedger, EventSink, Clock
L5 adapters     [provider-*, tools, store-sqlite, …: Phase 1+]

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
| `harness-context` *(Phase 5)* | types, protocol, ports |
| `harness-runtime` *(Phase 1)* | types, protocol, ports, context |
| `harness-testkit` | types, protocol, ports |
| `harness-cli` | anything except testkit |
| adapters *(later)* | anything except runtime, context, front-ends, testkit (`arch::adapter`) |

## Data flow (target)

1. A front-end sends an `Op` (for example `Op::Submit`).
2. The session actor (runtime) processes ops in order and runs the agent loop
   against `dyn Provider` and `dyn Tool`.
3. Every persisted `Event` is appended to the `EventLog` before any side effect
   that depends on it, then broadcast to front-ends as an `Envelope`.

## Cross-cutting rules already in code

- `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` in every crate.
- Workspace lints deny `unwrap`, `print!`/`eprint!`, `dbg!`, `todo!` and `unimplemented!` outside tests.
- Credentials never appear in config: secret fields are `keyring:` / `env:` references.
- Logs go to a daily-rotated file, never to the terminal.
