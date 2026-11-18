//! Turns events into terminal output and tracks the outcome for the exit code.

use std::io::{self, Write};

use harness_protocol::{Envelope, Event, TaskState};
use harness_types::{ContentBlock, StopReason, TaskId};
use serde_json::Value;

/// Exit codes of `harness run` (Tech spec 3.16).
pub mod exit {
    /// The task completed.
    pub const OK: u8 = 0;
    /// An error stopped the task.
    pub const ERROR: u8 = 1;
    /// The turn budget ran out.
    pub const BUDGET: u8 = 2;
    /// The model declined the request.
    pub const REFUSED: u8 = 3;
    /// The task completed, but actions needing approval were denied (`--approval never`).
    pub const APPROVAL_REQUIRED: u8 = 4;
    /// Interrupted (Ctrl-C) or cancelled.
    pub const INTERRUPTED: u8 = 130;
}

/// Watches events and works out the exit code.
///
/// It follows one task: the one passed to [`Outcome::watch`] (resume, cancel),
/// or else the first task whose state changes (a new prompt). Events of other
/// tasks do not end the run.
#[derive(Debug, Default)]
pub struct Outcome {
    task: Option<TaskId>,
    final_state: Option<TaskState>,
    task_started: bool,
    refused: bool,
    denied_without_asking: bool,
    error_before_task: bool,
}

impl Outcome {
    /// Follows `task` from now on.
    pub fn watch(&mut self, task: TaskId) {
        self.task = Some(task);
    }

    /// The task being followed, once known.
    #[must_use]
    pub fn task(&self) -> Option<TaskId> {
        self.task
    }

    /// The state the followed task ended in, once it has.
    #[must_use]
    pub fn final_state(&self) -> Option<TaskState> {
        self.final_state
    }

    /// Records one event.
    pub fn observe(&mut self, event: &Event) {
        match event {
            Event::TaskState { task, to, .. } => {
                if *self.task.get_or_insert(*task) != *task {
                    return;
                }
                self.task_started = true;
                if matches!(
                    to,
                    TaskState::Completed
                        | TaskState::Failed
                        | TaskState::Cancelled
                        | TaskState::BudgetExhausted
                        | TaskState::Paused
                ) {
                    self.final_state = Some(*to);
                }
            }
            Event::TurnFinished {
                stop: StopReason::Refusal { .. },
                ..
            } => self.refused = true,
            Event::Warning { kind, .. } if kind == "approval_denied" => {
                self.denied_without_asking = true;
            }
            // An error before any task started (busy, bad input) ends the run.
            Event::Error { .. } if !self.task_started => self.error_before_task = true,
            _ => {}
        }
    }

    /// `true` once the task has ended (completed, failed, paused, cancelled, out
    /// of budget) or an error stopped it from starting.
    #[must_use]
    pub fn finished(&self) -> bool {
        self.final_state.is_some() || self.error_before_task
    }

    /// The exit code for what was observed.
    #[must_use]
    pub fn exit_code(&self) -> u8 {
        match self.final_state {
            Some(TaskState::Completed) if self.denied_without_asking => exit::APPROVAL_REQUIRED,
            Some(TaskState::Completed) => exit::OK,
            Some(TaskState::Failed) if self.refused => exit::REFUSED,
            Some(TaskState::BudgetExhausted) => exit::BUDGET,
            Some(TaskState::Paused | TaskState::Cancelled) => exit::INTERRUPTED,
            _ => exit::ERROR,
        }
    }
}

/// Writes events for people (`stdout`: the answer; `stderr`: everything else)
/// or as JSON lines (`--json`: every envelope on `stdout`).
#[derive(Debug)]
pub struct Renderer {
    json: bool,
    /// Whether the last thing written to stdout ended with a newline.
    at_line_start: bool,
}

impl Renderer {
    /// A renderer in human (`json = false`) or JSON mode.
    #[must_use]
    pub fn new(json: bool) -> Self {
        Self {
            json,
            at_line_start: true,
        }
    }

    /// Renders one envelope.
    ///
    /// # Errors
    /// Any write error (for example a closed pipe).
    pub fn render(
        &mut self,
        envelope: &Envelope,
        out: &mut dyn Write,
        err: &mut dyn Write,
    ) -> io::Result<()> {
        if self.json {
            serde_json::to_writer(&mut *out, envelope)?;
            writeln!(out)?;
            return out.flush();
        }
        match &envelope.event {
            Event::TextDelta { text, .. } => {
                write!(out, "{text}")?;
                out.flush()?;
                if !text.is_empty() {
                    self.at_line_start = text.ends_with('\n');
                }
            }
            Event::TurnFinished { .. } | Event::AssistantAborted { .. } => self.end_line(out)?,
            Event::ThinkingDelta { text, .. } => write!(err, "{text}")?,
            Event::ToolCall { name, input, .. } if name == "todo" => {
                self.end_line(out)?;
                writeln!(err, "▸ todo")?;
                for item in input["items"].as_array().into_iter().flatten() {
                    let mark = match item["status"].as_str() {
                        Some("completed") => "[x]",
                        Some("in_progress") => "[~]",
                        _ => "[ ]",
                    };
                    writeln!(err, "  {mark} {}", item["text"].as_str().unwrap_or(""))?;
                }
            }
            Event::ToolCall { name, input, .. } => {
                self.end_line(out)?;
                writeln!(err, "▸ {name}: {}", summarize_input(name, input))?;
            }
            Event::ToolFinished { ok, sent, .. } => {
                writeln!(err, "  {} {}", if *ok { "✓" } else { "✗" }, last_line(sent))?;
            }
            Event::Warning { message, .. } => writeln!(err, "warning: {message}")?,
            Event::UndoApplied { paths, .. } => {
                writeln!(err, "↺ restored {}", paths.join(", "))?;
            }
            Event::Error { message, hint, .. } => {
                self.end_line(out)?;
                writeln!(err, "error: {message}")?;
                if let Some(hint) = hint {
                    writeln!(err, "hint: {hint}")?;
                }
            }
            Event::TaskState { to, reason, .. } => match to {
                TaskState::Failed => writeln!(err, "task failed: {reason}")?,
                TaskState::BudgetExhausted => writeln!(err, "stopped: {reason}")?,
                // A requested pause is reported by the driver, with how to resume.
                TaskState::Paused if reason == "paused" => {}
                TaskState::Paused => writeln!(err, "interrupted")?,
                TaskState::Cancelled => writeln!(err, "cancelled")?,
                _ => {}
            },
            _ => {}
        }
        err.flush()
    }

    fn end_line(&mut self, out: &mut dyn Write) -> io::Result<()> {
        if !self.at_line_start {
            writeln!(out)?;
            self.at_line_start = true;
        }
        Ok(())
    }
}

/// Cuts `text` to `max` characters on one line.
fn shorten(text: &str, max: usize) -> String {
    let one_line = text.replace('\n', " ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    }
}

/// One-line summary of a tool call's input, for example the shell command.
#[must_use]
pub fn summarize_input(name: &str, input: &Value) -> String {
    let field = match name {
        "shell" => input["command"].as_str().map(str::to_owned),
        "read_file" | "write_file" | "edit_file" => input["path"].as_str().map(str::to_owned),
        "glob" | "grep" => input["pattern"].as_str().map(str::to_owned),
        "ask_user" => input["question"].as_str().map(str::to_owned),
        "propose_plan" => Some("a plan for your approval".to_owned()),
        "apply_patch" => input["patch"]
            .as_str()
            .and_then(|p| harness_tools::patch::parse_patch(p).ok())
            .map(|p| p.paths().join(", ")),
        _ => None,
    };
    shorten(&field.unwrap_or_else(|| input.to_string()), 120)
}

/// The last non-empty line of a tool result, shortened.
fn last_line(content: &[ContentBlock]) -> String {
    let text: String = content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n");
    shorten(
        text.lines()
            .rev()
            .map(str::trim)
            .find(|l| !l.is_empty() && *l != "</untrusted>")
            .unwrap_or(""),
        100,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{Message, Risk, Role, Seq, SessionId, TaskId, Usage};
    use serde_json::json;

    fn env(event: Event) -> Envelope {
        Envelope {
            seq: Seq(1),
            ts: 0,
            session: SessionId::new(),
            event,
        }
    }

    fn render(r: &mut Renderer, events: Vec<Event>) -> (String, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        for e in events {
            r.render(&env(e), &mut out, &mut err).unwrap();
        }
        (
            String::from_utf8(out).unwrap(),
            String::from_utf8(err).unwrap(),
        )
    }

    fn state(to: TaskState) -> Event {
        Event::TaskState {
            task: TaskId::new(),
            from: TaskState::Running,
            to,
            reason: "r".into(),
        }
    }

    #[test]
    fn human_mode_streams_the_answer_to_stdout_and_activity_to_stderr() {
        let t = TaskId::new();
        let (out, err) = render(
            &mut Renderer::new(false),
            vec![
                Event::ToolCall {
                    task: t,
                    id: "c".into(),
                    name: "shell".into(),
                    input: json!({"command": "ls -la"}),
                    risk: Risk::Exec {
                        command: "ls -la".into(),
                    },
                },
                Event::ToolFinished {
                    call_id: "c".into(),
                    ok: true,
                    full: harness_protocol::BlobOrText::Text {
                        text: String::new(),
                    },
                    sent: vec![ContentBlock::text("a\nb\n[exit code 0]")],
                },
                Event::TextDelta {
                    task: t,
                    text: "Two ".into(),
                },
                Event::TextDelta {
                    task: t,
                    text: "files.".into(),
                },
                Event::TurnFinished {
                    task: t,
                    stop: StopReason::EndTurn,
                },
            ],
        );
        assert_eq!(out, "Two files.\n");
        assert!(err.contains("▸ shell: ls -la"), "{err}");
        assert!(err.contains("✓ [exit code 0]"), "{err}");
    }

    #[test]
    fn errors_and_warnings_go_to_stderr_with_hints() {
        let (out, err) = render(
            &mut Renderer::new(false),
            vec![
                Event::Warning {
                    kind: "k".into(),
                    message: "careful".into(),
                },
                Event::Error {
                    kind: "provider_auth".into(),
                    message: "bad key".into(),
                    hint: Some("store it".into()),
                },
                Event::ToolFinished {
                    call_id: "c".into(),
                    ok: false,
                    full: harness_protocol::BlobOrText::Text {
                        text: String::new(),
                    },
                    sent: vec![ContentBlock::text("denied")],
                },
                state(TaskState::Failed),
            ],
        );
        assert!(out.is_empty());
        for needle in [
            "warning: careful",
            "error: bad key",
            "hint: store it",
            "✗ denied",
            "task failed: r",
        ] {
            assert!(err.contains(needle), "missing {needle:?} in {err}");
        }
    }

    #[test]
    fn json_mode_prints_one_envelope_per_line() {
        let t = TaskId::new();
        let (out, err) = render(
            &mut Renderer::new(true),
            vec![
                Event::TextDelta {
                    task: t,
                    text: "hi".into(),
                },
                Event::Usage {
                    task: t,
                    usage: Usage::default(),
                    cost_micros: 0,
                },
            ],
        );
        assert!(err.is_empty());
        let lines: Vec<Envelope> = out
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert!(matches!(lines[0].event, Event::TextDelta { .. }));
    }

    #[test]
    fn input_summaries_pick_the_useful_field() {
        assert_eq!(
            summarize_input("shell", &json!({"command": "cargo test"})),
            "cargo test"
        );
        assert_eq!(
            summarize_input("read_file", &json!({"path": "src/main.rs", "offset": 10})),
            "src/main.rs"
        );
        let other = summarize_input("mcp__x__y", &json!({"q": "a".repeat(300)}));
        assert!(other.chars().count() <= 121, "{other}");
        for (name, input, want) in [
            (
                "write_file",
                json!({"path": "a.rs", "content": "x"}),
                "a.rs",
            ),
            (
                "edit_file",
                json!({"path": "b.rs", "old": "x", "new": "y"}),
                "b.rs",
            ),
            ("glob", json!({"pattern": "**/*.rs"}), "**/*.rs"),
            (
                "grep",
                json!({"pattern": "TODO", "mode": "content"}),
                "TODO",
            ),
            ("ask_user", json!({"question": "Which one?"}), "Which one?"),
            (
                "apply_patch",
                json!({"patch": "*** Begin Patch\n*** Update File: a.rs\n@@\n-x\n+y\n*** Add File: b.rs\n+1\n*** End Patch"}),
                "a.rs, b.rs",
            ),
        ] {
            assert_eq!(summarize_input(name, &input), want, "{name}");
        }
    }

    #[test]
    fn todo_calls_render_as_a_checklist() {
        let (_, err) = render(
            &mut Renderer::new(false),
            vec![Event::ToolCall {
                task: TaskId::new(),
                id: "t".into(),
                name: "todo".into(),
                input: json!({"items": [
                    {"text": "read", "status": "completed"},
                    {"text": "fix", "status": "in_progress"},
                    {"text": "test", "status": "pending"}
                ]}),
                risk: Risk::ReadOnly { paths: vec![] },
            }],
        );
        assert_eq!(err, "▸ todo\n  [x] read\n  [~] fix\n  [ ] test\n");
    }

    #[test]
    fn result_lines_skip_the_untrusted_wrapper() {
        let sent = vec![ContentBlock::text(
            "<untrusted source=\"file:a\">\n     1\thello\n</untrusted>",
        )];
        assert_eq!(last_line(&sent), "1\thello");
    }

    #[test]
    fn exit_codes_follow_the_final_state() {
        let cases = [
            (vec![state(TaskState::Completed)], exit::OK),
            (vec![state(TaskState::Failed)], exit::ERROR),
            (vec![state(TaskState::BudgetExhausted)], exit::BUDGET),
            (vec![state(TaskState::Paused)], exit::INTERRUPTED),
            (vec![state(TaskState::Cancelled)], exit::INTERRUPTED),
            (
                vec![
                    Event::TurnFinished {
                        task: TaskId::new(),
                        stop: StopReason::Refusal { category: None },
                    },
                    state(TaskState::Failed),
                ],
                exit::REFUSED,
            ),
            (
                vec![
                    Event::Warning {
                        kind: "approval_denied".into(),
                        message: String::new(),
                    },
                    state(TaskState::Completed),
                ],
                exit::APPROVAL_REQUIRED,
            ),
            (
                vec![Event::Error {
                    kind: "busy".into(),
                    message: String::new(),
                    hint: None,
                }],
                exit::ERROR,
            ),
        ];
        for (events, code) in cases {
            let mut o = Outcome::default();
            events.iter().for_each(|e| o.observe(e));
            assert_eq!(o.exit_code(), code, "{events:?}");
        }
    }

    #[test]
    fn a_requested_pause_is_not_called_an_interruption() {
        let paused = |reason: &str| Event::TaskState {
            task: TaskId::new(),
            from: TaskState::Running,
            to: TaskState::Paused,
            reason: reason.into(),
        };
        let (_, err) = render(&mut Renderer::new(false), vec![paused("paused")]);
        assert_eq!(err, "");
        let (_, err) = render(&mut Renderer::new(false), vec![paused("interrupted")]);
        assert_eq!(err, "interrupted\n");
    }

    #[test]
    fn only_the_watched_task_ends_the_run() {
        let mut o = Outcome::default();
        let mine = TaskId::new();
        o.watch(mine);
        o.observe(&Event::TaskState {
            task: TaskId::new(),
            from: TaskState::Paused,
            to: TaskState::Cancelled,
            reason: String::new(),
        });
        assert!(!o.finished());
        o.observe(&Event::TaskState {
            task: mine,
            from: TaskState::Running,
            to: TaskState::Paused,
            reason: String::new(),
        });
        assert_eq!(o.final_state(), Some(TaskState::Paused));
        assert_eq!(o.exit_code(), exit::INTERRUPTED);
    }

    #[test]
    fn finished_after_a_terminal_state_or_an_early_error() {
        let mut o = Outcome::default();
        o.observe(&Event::AssistantMessage {
            task: TaskId::new(),
            message: Message::new(Role::Assistant, vec![]),
        });
        assert!(!o.finished());
        let task = TaskId::new();
        let to = |to| Event::TaskState {
            task,
            from: TaskState::Running,
            to,
            reason: String::new(),
        };
        o.observe(&to(TaskState::AwaitingApproval));
        assert!(!o.finished());
        o.observe(&to(TaskState::Completed));
        assert!(o.finished());
        let mut early = Outcome::default();
        early.observe(&Event::Error {
            kind: "segment".into(),
            message: String::new(),
            hint: None,
        });
        assert!(early.finished());
    }
}
