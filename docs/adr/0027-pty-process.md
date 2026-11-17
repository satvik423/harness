# ADR-027: PTY tools on `pty-process`

- **Status:** Accepted
- **Date:** 2026-11-17
- **Source:** Decision by the user during Phase 2 (P2-T9), 17 Nov 2026

## Context

Tech spec 3.1 and P2-T9 name `portable-pty` for the PTY tools. When Phase 2 started,
its last release (0.9.0) was from February 2025, 21 months earlier. It pulls in an old
`nix` 0.28 (a second copy next to the workspace's 0.32), plus `serial2`, `downcast-rs`,
`filedescriptor` and `anyhow`, and its reads block, so every program would need its
own blocking thread. Base rule 2 asks for actively maintained, up-to-date libraries.

`pty-process` 0.5.4 (MIT, released October 2026) has one runtime dependency (`rustix`)
and a tokio-native async API (`AsyncRead`/`AsyncWrite` on the pty, `tokio::process::Child`
for the program). It is Unix-only. The `shell` tool is already Unix-only (it kills
process groups with `killpg`), and Windows is P2 "without sandbox" in NFR-06.

The spec's tool table gives all four PTY tools the risk `Exec`. Reading buffered
output and stopping a program the agent started change nothing outside the session,
but as `Exec` they would ask for approval on every poll when commands are not sandboxed.

## Decision

- The PTY tools use `pty-process` with the `async` feature.
- Each program runs `/bin/sh -c` as a session leader on its own pty, so its process
  group is its pid; `pty_kill` and dropping the `PtyManager` kill the whole group.
- `pty_start` and `pty_write` have risk `Exec` (typing into a REPL can run anything).
  `pty_read` and `pty_kill` have risk `ReadOnly`.

## Consequences

The PTY tools are not available on Windows (like `shell`). When Windows support is
built (P2), it needs a ConPTY-based implementation behind the same tools.
