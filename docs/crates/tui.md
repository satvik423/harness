# harness-tui

The terminal UI (Phase 4, Tech spec 3.16), built purely on the session event stream
(guardrail 15). A front-end crate: it depends only on types, protocol and ports; the
`cli` crate wires it to the runtime through the `Backend` trait.

## Public API

| Item | Purpose |
|---|---|
| `run(backend, attached, TuiOptions)` | Runs the terminal loop: raw mode, alternate screen, bracketed paste, focus events; restores the terminal on exit and on panic |
| `Backend` | What the TUI needs: `send(Op)`, `tasks()`, `changes()`, `new_session(model, effort)`, `sessions()`, `open_session(id)`, `models()` |
| `Attached`, `SessionView`, `ChangedFile`, `SessionSummary` | A session to show: its view (id, persona, model, effort, permission mode, whether `full` is allowed, context limit, priced), live events, stored history and the project's earlier prompts; a stored session as `/sessions` lists it |
| `App` | All state and transitions, free of I/O: `apply(&Envelope) -> Vec<Effect>`, `key(KeyEvent) -> Vec<Action>`, `mouse(MouseEvent)`, `paste(&str)`, `command(Slash)`, `hint_last_session` |
| `Overlay`, `Pick` | A panel over the transcript; pickers (`/sessions`, `/model`, `/mode`) carry one `Action` per pickable line |
| `view` | Drawing, plus overlays (`help`, `cost`, `context`, `tasks`, `diff`, `models`, `modes`, `sessions`) |
| `markdown::render` | Markdown to styled lines (pulldown-cmark; code blocks highlighted with syntect) |
| `diff::unified`, `diff::patch_lines` | Unified diffs (similar) and coloured patches |
| `ledger` | Token and cost totals per turn, task and session; `tokens`, `money` formatting |
| `slash::parse`, `slash::COMMANDS` | Slash commands |
| `paste::attachments` | Pasted text that names existing files (quoted, escaped, `file://`) becomes attachments |
| `notify::bytes` | Bell + OSC 9 notification, sent when the terminal is not focused |

## Layout

```
 harness  default · openai/gpt-6-luna · high · seg 0 · auto · 01KAB3XY
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
- Finished replies keep their rendered Markdown (`Item::Assistant::rendered`), so only
  the streaming reply is rendered on each frame.
- Finished shell cards keep the last five lines of output; other tools keep one.
- Prompt history: ↑ on the first line of the input recalls older prompts, ↓ on the last
  line newer ones and then the draft. The backend loads it from the store (the user's
  own words from `UserInput` events), so it survives restarts with no extra file.
- The mouse wheel scrolls the open panel or the transcript. Capture is on unless
  `ui.mouse = false` (`TuiOptions.mouse`); the terminal then needs Shift+drag to select.
- Screens are tested with `TestBackend` and `insta` snapshots.
- **Modes (ADR-042).** Shift+Tab and `/mode` send `Op::SetMode`; the header changes only
  when `ModeChanged` arrives, so it never shows a mode that is not in force. A plan from
  `propose_plan` is drawn in the transcript as Markdown, and the question panel asks only
  for the decision. `/clear` and `/model` carry the current mode into the new session.
- **Approval descriptions (ADR-043).** The panel shows the model's `summary` in bold
  above the policy's reason, which names the exact command or files.
