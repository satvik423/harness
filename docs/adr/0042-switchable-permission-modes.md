# ADR-042: Permission modes switch while a session runs, and plan mode

- **Status:** Accepted
- **Date:** 2026-11-18
- **Source:** Phase 4 trial follow-up 2. The user asked for modes like other harnesses
  (manual, accept edits, plan) and decided to keep the profile names `read-only` and
  `auto`, add `plan`, let commands ask in plan mode, and leave an AI-judged "auto" to the
  Phase 13 sandbox.

## Context

Profiles (`read-only`, `auto`, `ci`, `full`, Tech spec 3.6) were chosen once, at startup.
There was no way to tighten or loosen permissions while working, and no mode where the
agent investigates and proposes before it changes anything. The system prompt and the
tools are frozen for a segment (ADR-003, ADR-032), so a mode switch must not edit them.

## Decision

- **`harness_types::Mode`**: `read-only`, `auto`, `plan`, `full`. It belongs to the
  session. Every `PolicyCall` carries the mode in force, and the policy judges the call
  by it. `Profile` stays the startup setting and maps to a starting mode (`ci` is `auto`
  that never asks).
- **What each mode lets run without asking:**
  - `read-only`: reads.
  - `auto`: reads and edits inside the project.
  - `plan`: reads. Edits are **denied**, before any rule is matched, so no allow rule or
    grant overrides plan mode. Commands, network and anything else still ask.
  - `full`: everything except built-in deny paths. It counts only when the process
    started with `--dangerously-full-access`; otherwise a call in `full` is judged as
    `auto`, and the runtime refuses to switch to it.
- **Switching:** `Op::SetMode`. The TUI sends it on Shift+Tab (read-only → auto → plan,
  then `full` only when allowed) and on `/mode`. The runtime records
  `Event::ModeChanged { from, to, note }`, then changes the mode. The mode is read on
  every call, so a switch during a task applies from the next call.
- **Telling the model:** the event's `note` is sent as a system note at the start of the
  next user message, so nothing already sent changes. A note is queued only while a
  segment is open. A new segment's environment states the current mode instead, and
  replay does exactly the same. `SessionOpened` records the starting mode.
- **Resuming:** a session keeps its mode (from `SessionOpened`, then the last
  `ModeChanged`). `--mode` on resume switches it. A session left in `full` drops to
  `auto`, with a warning, in a process started without the flag.
- **Plan approval:** a new `propose_plan { plan }` tool, read-only. In plan mode it shows
  the plan and offers three choices:
  - "Yes, and auto-accept edits" switches to `auto`.
  - "Yes, but ask before each edit" switches to `read-only`.
  - "No, keep planning" stays in plan mode; typed words go back to the model as feedback.
  It switches modes through a small `ModeControl` port on `ToolCtx`. In headless runs
  (nobody to answer, or a closed input) it tells the model to end with the plan. Outside
  plan mode it does nothing.
- **New sessions** from `/clear` and `/model` keep the mode the user chose. A stored
  session opened with `/sessions` keeps its own.

## Consequences

Users can move between careful and fast modes at any time. The transcript stays
append-only: a switch adds a note; it never rewrites anything. A real "auto" mode, where
commands run without asking, arrives with the Phase 13 sandbox. Until then `auto` still
asks before commands. Personas (Phase 6) will set a starting profile per persona.
