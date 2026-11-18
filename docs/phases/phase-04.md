# Phase 04: Terminal UI

## Summary

`harness` now opens a terminal UI built purely on the session event stream. It has:

- a streaming Markdown transcript with highlighted code;
- tool cards with live output, diff previews and checklists;
- an approval panel with diff previews and keyboard shortcuts, and `ask_user` questions;
- file attachments by paste or drag;
- slash commands;
- a status bar with context, cache, cost and task state;
- a notification when the terminal is in the background.

Costs are computed per turn, task and session. `harness debug last-request` and
`diff-requests` show the exact provider bodies and make the append-only property visible.

**Exit criterion:** one full working day on a real project without falling back to
another tool. By your decision, this is your trial after my checks. Everything I can
verify is done: 618 tests, a scripted live session in a real terminal (tmux) with
gpt-6-luna, and all checks. Phase 4 is approved after your trial; fixes from it go into a
follow-up commit.

## Tasks

| ID | Status | Notes |
|---|---|---|
| P4-T1 | Done | New `harness-tui` crate: header, transcript, input box (`ratatui-textarea`, ADR-037), status bar. The event loop combines crossterm `EventStream`, session events and a 120 ms tick. The terminal is restored on exit and on panic |
| P4-T2 | Done | Streaming text; tool cards with a spinner, live output tail, result line, `+/−` counts; diff previews (`edit_file` old→new, `write_file` against the file on disk, `apply_patch` coloured); `todo` checklists |
| P4-T3 | Done | Approval panel `[o]nce [s]ession [a]lways [p]roject [d]eny` with a diff preview; a deny reason is typed inline; key hints sit in the bottom border. Questions take a number (several for multi) or typed words. Bell + OSC 9 notification when unfocused |
| P4-T4 | Done | `/help /model /effort /pause /resume /tasks /diff /undo /clear /cost /context /quit`. `/model` and `/effort` start a new session after a confirmation (ADR-038). `/diff` compares each file's first snapshot with the file now |
| P4-T5 | Done | Pasted or dropped paths (plain, quoted, escaped, `file://`) become 📎 attachments sent as `InputItem::File`; the runtime adds a reference note (ADR-039) |
| P4-T6 | Done | `ModelPrice` in the runtime fills `Usage.cost_micros` from `[pricing]`. Status bar shows `ctx used/limit · cache % · $cost · task state`; `/cost` per turn, task and session (FR-PRV-06); `/context`. Unpriced models show `$?` with a config hint |
| P4-T7 | Done | Markdown with `pulldown-cmark` (headings, emphasis, lists, quotes, links, tables, task lists) and `syntect` code highlighting (pure-Rust regex build) |
| P4-T8 | Done | `harness debug last-request` (exact adapter-built body, key-shaped strings redacted) and `diff-requests` (append-only verdict plus line diff), from `request_history` over the log (ADR-040) |

## Requirements covered

| Requirement | How |
|---|---|
| FR-UI-02 | ratatui TUI with approvals, slash commands, attachments and a status bar |
| FR-PRV-06 | Token and cost accounting per turn, task and session in `/cost` and the status bar; costs stored in `Usage` events. Per persona needs personas (Phase 6) |
| NFR-12 (part) | `harness debug last-request` with redaction; `diff-requests` |

## Exit criteria

**"You use it for a full working day on a real project without falling back to another
tool."** This is pending your trial (your decision). What I verified:

- **Automated:** `harness-tui` has 37 unit and snapshot tests plus 8 doctests:
  - `App` state transitions: submit, attachments, streaming, tool cards, previews, approvals, questions, interrupt, slash commands, history;
  - screens: snapshot of the main screen, overlays, scrolling, approval keys with a wrapping reason.
- **CLI backend:** 2 tests open, run, list tasks and changes, and reopen with history and locking.
- **Live in a real terminal (tmux, 110×34) with `openai/gpt-6-luna`:**
  - A prompt fixed a bug: tool cards, `+1 −1` diff count, reply, status `2.1k/250.0k ctx · cache 73%`.
  - A shell call showed the approval panel; `o` ran it.
  - `/diff` showed the change.
  - `/cost` showed 6 turns with per-task and per-turn rows.
  - `/undo` restored the file.
  - `/model openai/gpt-6-luna` asked for confirmation and started a new session.
  - Ctrl-D exited cleanly, with empty stderr.
  - `harness --session ID` showed the stored history, undo included.
  - With the `read-only` profile, an `edit_file` approval showed its diff. The live check found that a long, wrapped reason pushed the key hints out of the panel (fixed test-first: the panel is sized by its wrapped height and the hints are in the border). Then `d` + "not now" sent the reason to the model.
  - A dropped file (bracketed paste) became a 📎 chip; the model edited it after `o`.
  - `harness debug diff-requests` on that session: `append-only: yes — same system prompt, tools and model; the 5 earlier messages are unchanged and 2 were appended`.

Checks (all pass):

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 618 passed |
| `cargo test --workspace --doc` | 56 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 90.9% lines |

## Tests added and coverage

| Crate | Tests (total) | Line coverage |
|---|---|---|
| harness-runtime | 57 (+4: pricing, attachments, request history) | **89.7%** (target 80%) |
| harness-context | 35 | **97.7%** (target 80%) |
| harness-policy | 41 | **92.9%** (target 80%) |
| harness-tui (new) | 37 + 8 doctests | 78.0% (`run.rs`, the terminal loop, is covered by the live run only) |
| harness-cli | 72 (+9: debug, factory, TUI backend, binary) | 85.1% |
| others | unchanged except store (+1 assertion set) | 91–100% |
| **Total** | **618** + 56 doctests (Phase 3: 568 + 46) | **90.9%** |

## New dependencies

All are the latest stable releases on 18 Nov 2026. `cargo deny` is clean (`bincode` and
`yaml-rust` come in through `syntect` with no flagged advisory).

| Crate | Version | Why |
|---|---|---|
| `ratatui` | 0.30.2 (`crossterm`, `unstable-rendered-line-info`) | The TUI; `line_count` gives exact wrapped heights for scrolling |
| `crossterm` | 0.29.0 (`events`, `event-stream`, `bracketed-paste`) | Terminal input, paste, focus events. The last release was April 2025, but it is ratatui's backend and has no maintained alternative |
| `ratatui-textarea` | 0.9.4 | The input box (ADR-037, user decision) |
| `pulldown-cmark` | 0.13.6 | Markdown parsing |
| `syntect` | 5.3.0 (`default-fancy`) | Code highlighting without C dependencies |
| `similar` | 3.2.1 | Diffs for previews, `/diff` and `diff-requests` |

## Decisions / ADRs

- **ADR-037** `ratatui-textarea` instead of `tui-textarea`. User decision.
- **ADR-038** `/model` and `/effort` apply to a new session. User decision.
- **ADR-039** Attached files become reference notes until Phase 11. User decision.
- **ADR-040** Costs in the runtime; debug bodies rebuilt from the log; `/diff` from first snapshots; the TUI's `Backend` trait.
- Smaller choices:
  - `harness` prints help (with a note) when not on a terminal.
  - The TUI allows 100 model calls per task.
  - The context limit shown is `context.compact_at_tokens`.
  - Esc and Ctrl-C interrupt a running task.

## Deviations from the plan (and user approval)

| Change | Approval |
|---|---|
| `ratatui-textarea` instead of `tui-textarea` (ADR-037) | User, at the start of the phase |
| `/model`, `/effort` start a new session until Phase 5 (ADR-038) | User, at the start of the phase |
| Attachments are reference notes until Phase 11 (ADR-039) | User, at the start of the phase |
| The exit criterion is checked by your day-long trial after my verification | User, at the start of the phase |

## Known issues

- Not yet verified in practice: a full working day (your trial).
- Focus events and OSC 9 notifications depend on the terminal (tmux forwards neither by default; a bell still rings).
- ~~The shell card shows only the last line~~: it keeps five lines (trial follow-up).
- ~~No OpenAI prices~~: the gpt-6 models have built-in prices (trial follow-up). Prompts over 272K input tokens cost more at OpenAI; the table does not model that.
- ~~Markdown re-rendered every frame~~: finished replies are cached (trial follow-up).
- A tool refused by the policy shows its reason twice: on the card and as a warning notice (the runtime records a `Warning` event too).
- With mouse capture on, selecting text needs Shift+drag (`[ui] mouse = false` turns capture off).
- `ratatui`'s `unstable-rendered-line-info` API may change in a minor release.
- In plan mode a command you approve can still change files (for example `sed -i`): plan
  mode refuses the edit tools, and commands are your call. The Phase 13 sandbox can make
  them read-only (follow-up 2).
- Approval descriptions are written by the model; the exact command is always shown under
  them (ADR-043).

## Trial follow-up: gaps filled before Phase 5

During your trial you could not get back to a saved session: the TUI showed no session
id and had no command for it. You asked me to audit everything left on purpose or
unfinished in Phases 0–4 and fill the gaps before Phase 5. This follow-up commit does
that.

| Change | Why | Evidence |
|---|---|---|
| `harness -c` / `--continue` opens the most recently active session | Getting back without knowing an id | binary test; live in tmux |
| `/sessions`: the project's sessions, most recently active first; ↑↓ Enter or a number switches | Same | `App` and view tests, backend test; live |
| A new session's first line names the last one and how to return | Same | `App` test; live |
| The header ends with the session's short id | You could not find the id | view test, snapshot |
| `harness session delete ID [--yes]` (ADR-041) | Spec 3.16 lists it; no phase task built it | store tests (rows, blobs, locks, forks), binary test; live |
| Sessions nobody typed into are deleted when the TUI leaves them | Every start opened one, so empty sessions piled up | backend test; live |
| Finished shell cards keep their last five output lines | Only `[exit code 0]` was left | `App` test; live |
| Finished replies keep their rendered Markdown | Every frame re-rendered every reply | view test |
| ↑ / ↓ prompt history, from the store (no new file) | Daily-driver basic (your choice) | `App` tests, store test, backend test; live |
| Mouse-wheel scrolling; `[ui] mouse = false` frees the mouse | Your choice; Shift+drag selects while it is on | `App` test, config test |
| `/model` lists models to pick from | Same picker as `/sessions`, at no cost | `App` and view tests |
| Built-in prices for OpenAI's gpt-6 models | `$?` for gpt-6-luna (your choice) | config test; live status bar showed `$0.0001` |
| Phase 1 known issues marked resolved | They were fixed in Phases 2–4 | `phase-01.md` |

The OpenAI prices are the Standard tier for prompts up to 272K input tokens. They come
from https://developers.openai.com/api/docs/pricing and the gpt-6-luna model page, both
read on 18 Nov 2026. gpt-6-luna: input $0.10, cached input $0.01, cache write $0.125,
output $0.50 per million tokens. They are fixed in the code and change only with an
update. Your `[pricing]` entries win.

New public items:

- `SessionStore::delete_session` and `DeletedSession` (ports);
- `SqliteStore::recent_inputs`;
- `SessionHandle::closed`;
- `Backend::sessions` / `open_session` and `SessionSummary`;
- `Overlay.picks` / `Pick`, `Action::OpenSession`, `Slash::Sessions`, `App::mouse`;
- `TuiOptions.notice` / `mouse`, `Attached.prompts`;
- `UiConfig.mouse`;
- `CliBackend::with_providers`, `recent_sessions`, `session_frame`.

No new dependencies.

**Checks after the follow-up (all pass):**

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 641 passed (+23) |
| `cargo test --workspace --doc` | 56 passed |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 91.4% lines: runtime 89.7%, context 97.7%, policy 92.9%, tui 80.7%, cli 88.6%, store 91.6% |

**Left for the phases that own them** (unchanged, see the plan):

- compaction, recall, fork (5);
- personas, `harness trust`, cost per persona (6);
- hooks and skills (7);
- subagents and queued tasks (8);
- several sessions in one process (10);
- attachment contents (11);
- sandbox, `harness doctor` (13);
- the NFR-01/02 performance measurements (15).

Anthropic still has no live test (no key).

## Trial follow-up 2: approval descriptions and permission modes

After the trial you asked for two things before Phase 5:

1. **A plain description on every approval.** You could not judge commands you did not
   understand from the command text alone.
2. **Modes like other harnesses.** There were none to switch: profiles were picked once,
   at startup, and Shift+Tab did nothing.

Your decisions:
- keep the names `read-only` and `auto`, and add `plan`;
- shell commands still ask in plan mode;
- no AI judge: a real "auto" comes with the Phase 13 sandbox.

ADR-042 and ADR-043 record them, and the plan is now v1.6.

| Change | Evidence |
|---|---|
| `shell`, `pty_start` and `pty_write` ask the model for a one-line `description`; approvals carry it as `summary` and show it **above** the exact command (TUI panel, headless prompt, notification, `session show`) (ADR-043) | tools, runtime, protocol, TUI, CLI tests; live: "Run test_calc.py with Python 3 to check whether it passes" above ``run `python3 test_calc.py` `` |
| Permission modes `read-only`, `auto`, `plan`, `full` belong to the session, and every policy call is judged in the mode in force (ADR-042) | policy mode × risk tests |
| Plan mode refuses every edit before rules are matched, so allow rules and grants cannot override it; commands still ask | policy test `plan_mode_beats_allow_rules_and_session_grants` |
| Shift+Tab cycles read-only → auto → plan (→ full only with `--dangerously-full-access`); `/mode [name]` switches or opens a picker | `App` tests; live |
| `Op::SetMode` records `ModeChanged`; the model gets a system note with its next message; a switch mid-task applies from the next call | runtime tests; live `debug diff-requests`: append-only, 17 earlier messages unchanged |
| `propose_plan` shows the plan in the transcript, with three choices: approve with auto-accept, approve and ask, or keep planning / type feedback | tools tests; live: plan, feedback, re-plan, approve → `auto`, edit ran without asking |
| A resumed session keeps its mode (`SessionOpened` records the start); `--mode` switches it; `full` drops to `auto` without the flag | persistence tests; live `harness -c` came back in `plan`, `-c --mode auto` switched |
| `harness --mode` and `harness run --mode`; `policy.profile = "plan"` | binary test, factory test |
| `harness run --mode plan` ends with the plan and changes nothing; a closed input counts as "nobody can approve" | tools test; live: one proposal, `calc.py` unchanged |
| `/clear` and `/model` keep the current mode | — |

The live run caught two problems, both fixed:
- With stdin closed, headless answered "(no answer…)", and `propose_plan` took it as
  feedback, so the model re-proposed three times.
- The plan card showed raw JSON.

New public items:

- `Mode` and `ParseModeError` (types);
- `ModeControl`, `FixedMode`, `ModeError`, `NO_ANSWER`, `PolicyCall.mode` and
  `ToolCtx.mode` (ports);
- `Op::SetMode`, `Event::ModeChanged`, `SessionOpened.mode` and
  `ApprovalRequested.summary` (protocol);
- `Profile::Plan`, `Profile::mode`, `PolicySettings.full_access` and `PLAN_MODE_REFUSAL`
  (policy);
- `SessionConfig.mode` and `full_access` (runtime);
- `ProposePlan`, `PLAN_CHOICES` and `DESCRIPTION_FIELD` (tools);
- `SessionView.mode` and `full_access`, `Slash::Mode` and `view::modes_overlay` (tui);
- `PolicyProfile::Plan` (config);
- `SessionFactory.mode`, `start_mode`, `RunOptions.mode`, `mode_given`, `full_access`
  and `WiringError::FullModeNotConfirmed` (cli).

No new dependencies.

Old sessions still load: the new event fields are optional. Sessions started before this
change keep their frozen tool list, so their approvals show no description.

**Checks after follow-up 2 (all pass):**

| Check | Result |
|---|---|
| `cargo fmt --all -- --check` | clean |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings` | clean |
| `cargo nextest run --workspace --all-features` | 674 passed (+33) |
| `cargo test --workspace --doc` | 58 passed (+2) |
| `RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps` | clean |
| `cargo deny check` | advisories ok, bans ok, licenses ok, sources ok |
| `cargo llvm-cov --workspace --summary-only` | 91.5% lines: runtime 89.6%, context 97.7%, policy 93.2%, tools 95.9%, tui 81.8%, cli 88.2% |

Known issue: in plan mode, a command you approve can still change files (for example
`sed -i`). Plan mode refuses the edit tools; commands are your call, which is why they
still ask. The Phase 13 sandbox can make plan-mode commands read-only.

## Notes for later phases

- **Phase 5:**
  - `/model` and `/effort` switch inside a session via compaction (ADR-038).
  - `/compact` and `/recall` slot into the slash framework.
  - `Compacted` and `Recalled` events need TUI rendering.
- **Phase 6:** persona in the header and `/persona`; cost per persona (FR-PRV-06);
  a persona's `policy profile` sets its starting mode (ADR-042).
- **Phase 7:** `/hooks`, `/skills`.
- **Phase 11:** replace attachment notes with converted content and images (ADR-039).
- **Phase 12:** fallbacks shown in the status bar.
- **Phase 13:** with the sandbox, `auto` runs sandboxed commands without asking (the
  real "auto" mode), and plan-mode commands can run read-only (ADR-042).
