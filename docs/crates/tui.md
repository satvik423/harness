# harness-tui

The terminal UI (Phase 4, Tech spec 3.16), built purely on the session event stream
(guardrail 15). A front-end crate: it depends only on types, protocol and ports; the
`cli` crate wires it to the runtime through the `Backend` trait.

## Public API

| Item | Purpose |
|---|---|
| `run(backend, attached, TuiOptions)` | Runs the terminal loop: raw mode, alternate screen, bracketed paste, focus events; restores the terminal on exit and on panic |
| `Backend` | What the TUI needs: `send(Op)`, `tasks()`, `changes()`, `new_session(model, effort)`, `models()` |
| `Attached`, `SessionView`, `ChangedFile` | A session to show: its view (persona, model, effort, profile, context limit, priced), live events and stored history |
| `App` | All state and transitions, free of I/O: `apply(&Envelope) -> Vec<Effect>`, `key(KeyEvent) -> Vec<Action>`, `paste(&str)`, `command(Slash)` |
| `view` | Drawing, plus overlays (`help`, `cost`, `context`, `tasks`, `diff`, `models`) |
| `markdown::render` | Markdown to styled lines (pulldown-cmark; code blocks highlighted with syntect) |
| `diff::unified`, `diff::patch_lines` | Unified diffs (similar) and coloured patches |
| `ledger` | Token and cost totals per turn, task and session; `tokens`, `money` formatting |
| `slash::parse`, `slash::COMMANDS` | Slash commands |
| `paste::attachments` | Pasted text that names existing files (quoted, escaped, `file://`) becomes attachments |
| `notify::bytes` | Bell + OSC 9 notification, sent when the terminal is not focused |

## Layout

```
 harness  default · openai/gpt-6-luna · high · seg 0 · auto
you › fix the login test
▸ edit_file src/auth.rs ⠋ (+1 −1)
    - x < y
    + x <= y
┌ Approve? ───────────────────────────────────────────┐
│write src/auth.rs                                    │
│    - x < y                                          │
│    + x <= y                                         │
└ [o]nce  [s]ession  [a]lways  [p]roject  [d]eny ─────┘
┌ › ──────────────────────────────────────────────────┐
│ Ask anything · /help for commands                   │
└─────────────────────────────────────────────────────┘
 41.5k/250.0k ctx · cache 97% · $0.42 · task waiting for you · Esc interrupt · ^P pause
```

## Design notes

- Events drive everything: user lines from `UserInput`, streaming text from `TextDelta`
  (replaced by the final `AssistantMessage`), tool cards from `ToolCall` / `ToolOutput` /
  `ToolFinished`, prompts from `ApprovalRequested` / `QuestionAsked`, which are dropped
  when the task leaves `AwaitingApproval` (paused or cancelled elsewhere).
- Stored history is applied without prompts, notifications or file previews.
- Diff previews read the file from disk when the call is announced (before it runs).
- Approval key hints live in the panel's bottom border, so a long reason can never
  hide them. The panel is sized by its wrapped height.
- Scrolling uses ratatui's `line_count` (feature `unstable-rendered-line-info`) for the
  exact wrapped height; the transcript follows the bottom until you scroll back.
- Screens are tested with `TestBackend` and `insta` snapshots.
