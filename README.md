# harness

A general-purpose, persona-driven agent harness written in Rust. It takes the
best ideas from Claude Code, Codex CLI and Hermes, and adds a context engine
that never loses details (append-only transcripts, anchored compaction, recall).

**Status:** Phase 0 (foundations) complete. See [`docs/phases/`](docs/phases/).

- Plan (PRD, design, spec, phases, guardrails): [`building-harness.html`](building-harness.html)
- Documentation: [`docs/`](docs/README.md)
- Decisions: [`docs/adr/`](docs/adr/README.md)
- Contributor rules: [`CLAUDE.md`](CLAUDE.md)

## Build and check

Requires `rustup`; the toolchain is pinned in `rust-toolchain.toml`.

```sh
cargo build
cargo nextest run --workspace --all-features   # or: cargo test --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo deny check
```

## Licence

Dual-licensed under [MIT](LICENSE-MIT) or [Apache-2.0](LICENSE-APACHE), at your option.
