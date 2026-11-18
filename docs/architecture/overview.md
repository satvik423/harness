# Architecture overview

This page tracks the *current* state of the code. The target architecture is in
Building Harness, SDD 2.3–2.4.

## Layers

```
L1 front-ends   harness-cli (binary: harness run)        [tui: Phase 4, server: Phase 15]
L2 protocol     harness-protocol   Op in → Event out, wrapped in Envelope{seq, ts, session}
L3 runtime      harness-runtime (session actor, turn engine), harness-context (requests, transcript rules)
L4 ports        harness-ports      Provider, Tool, ToolRegistry, EventLog, BlobStore, Sandbox, ReadLedger, EventSink, Clock
L5 adapters     harness-provider-anthropic, harness-provider-openai, harness-tools, harness-policy, harness-store-sqlite   [store-sqlite: Phase 3, …]

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
| `harness-tools`, `harness-provider-anthropic`, `harness-provider-openai`, `harness-policy`, `harness-store-sqlite` | anything except runtime, context, front-ends, testkit (`arch::adapter`) |
| `harness-testkit` | types, protocol, ports |
| `harness-tui` | types, protocol, ports, config |
| `harness-cli` | anything except testkit |
| adapters *(later)* | anything except runtime, context, front-ends, testkit (`arch::adapter`) |

## Data flow (Phases 3–4)

1. The CLI opens `.harness/state.db` (`SqliteStore`), builds the provider and the
   policy, and builds `RuntimeDeps` (provider, tools, store as event log, blob store,
   session store and snapshot store, policy). It takes the session's lock.
2. `spawn_session` registers a new session; `resume_session` rebuilds a stored one from
   its events (the frozen segment frame, then the messages) and recovers a task that
   was running when harness stopped.
3. The session actor runs one task at a time. Every persisted event goes through the
   `Journal`: numbered, committed to the event log, then broadcast (NFR-03).
4. The turn engine streams replies through `dyn Provider`, judges each tool call with
   `dyn Policy`, snapshots files before tools write them, runs the calls (parallel-safe
   reads together), caps each result once, and appends every message through
   `SegmentContext` (I1, I2). Pause requests take effect at safe points.
5. `harness run` renders events and answers prompts from stdin; `harness` opens the
   TUI, which renders the same event stream and sends ops through its `Backend`
   (implemented in `cli`). Other terminals can leave pause, resume or cancel requests
   in the store.

## Cross-cutting rules already in code

- `#![forbid(unsafe_code)]` and `#![deny(missing_docs)]` in every crate.
- Workspace lints deny `unwrap`, `print!`/`eprint!`, `dbg!`, `todo!` and `unimplemented!` outside tests.
- Credentials never appear in config: secret fields are `keyring:` / `env:` references.
- Logs go to a daily-rotated file, never to the terminal.
