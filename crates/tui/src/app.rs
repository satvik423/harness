//! [`App`]: the TUI's state and every transition, kept free of I/O so it can be
//! tested directly. Events from the session go in through [`App::apply`];
//! keys and pastes through [`App::key`] and [`App::paste`]. Both return what the
//! run loop must do ([`Action`], [`Effect`]).

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use harness_ports::Effort;
use harness_protocol::{Decision, Envelope, Event, InputItem, Op, RuleScope, TaskState};
use harness_types::{ApprovalId, ContentBlock, Message, ModelRef, QuestionId, TaskId};
use ratatui::text::Line;
use ratatui_textarea::TextArea;
use serde_json::Value;

use crate::backend::{Attached, SessionView};
use crate::diff::{DiffLine, patch_lines, unified};
use crate::ledger::Ledger;
use crate::slash::{Slash, parse};

/// Lines of a diff preview kept on a tool card and in the approval panel.
const PREVIEW_LINES: usize = 40;
/// Live output kept per tool card (the tail).
const LIVE_CHARS: usize = 4_000;

/// How serious a notice is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// Something to know.
    Info,
    /// Something went sideways.
    Warning,
    /// Something failed.
    Error,
}

/// A tool call as shown in the transcript.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCard {
    /// Call id.
    pub id: String,
    /// Tool name.
    pub name: String,
    /// One-line summary of the input.
    pub summary: String,
    /// Live output so far (tail).
    pub live: String,
    /// `None` while running, then whether it succeeded.
    pub ok: Option<bool>,
    /// The last line of the result.
    pub result: String,
    /// Diff preview for file changes.
    pub preview: Vec<DiffLine>,
    /// Checklist items for `todo` (`status`, `text`).
    pub todo: Vec<(String, String)>,
}

/// One entry of the transcript.
#[derive(Debug, Clone, PartialEq)]
pub enum Item {
    /// What the user sent.
    User {
        /// The prompt text.
        text: String,
        /// Attached file paths.
        attachments: Vec<String>,
    },
    /// The model's reply (Markdown).
    Assistant {
        /// Text so far.
        text: String,
        /// `false` while streaming.
        done: bool,
    },
    /// A reasoning summary.
    Thinking(String),
    /// A tool call.
    Tool(ToolCard),
    /// A message from harness.
    Notice {
        /// How serious.
        level: Level,
        /// The message.
        text: String,
    },
}

/// A question waiting for the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Prompt {
    /// Approve a tool call.
    Approval {
        /// The request.
        id: ApprovalId,
        /// What is asked about.
        reason: String,
        /// Diff preview, if the call changes files.
        preview: Vec<DiffLine>,
    },
    /// Typing the reason for a denial.
    Reason {
        /// The request.
        id: ApprovalId,
        /// What was typed so far.
        text: String,
    },
    /// An `ask_user` question.
    Question {
        /// The request.
        id: QuestionId,
        /// The question.
        question: String,
        /// The offered answers.
        options: Vec<String>,
        /// Several may be chosen.
        multi: bool,
        /// Which options are chosen.
        chosen: Vec<bool>,
    },
    /// A yes/no confirmation for something harness would do.
    Confirm {
        /// The question.
        question: String,
        /// What happens on yes.
        then: Box<Action>,
    },
}

/// A scrollable panel over the transcript (`/help`, `/cost`, `/diff`, …).
#[derive(Debug, Clone, PartialEq)]
pub struct Overlay {
    /// Title.
    pub title: String,
    /// Content.
    pub lines: Vec<Line<'static>>,
    /// First visible line.
    pub scroll: u16,
}

/// Something the run loop must do.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Send an op to the session.
    Send(Op),
    /// Run a slash command (some need the backend).
    Command(Slash),
    /// Start a new session with these settings.
    NewSession {
        /// Model.
        model: ModelRef,
        /// Effort.
        effort: Option<Effort>,
    },
    /// Leave harness.
    Quit,
}

/// A side effect of an event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Effect {
    /// Tell the user (bell and terminal notification) if the terminal is not focused.
    Notify(String),
}

/// Settings picked for the next session (`/model`, `/effort`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextSession {
    /// Model.
    pub model: ModelRef,
    /// Effort.
    pub effort: Option<Effort>,
}

/// The whole TUI state.
pub struct App {
    /// The current session.
    pub view: SessionView,
    /// The transcript.
    pub items: Vec<Item>,
    /// A question waiting for an answer.
    pub prompt: Option<Prompt>,
    /// A panel over the transcript.
    pub overlay: Option<Overlay>,
    /// Token and cost accounting.
    pub ledger: Ledger,
    /// The latest task and its state.
    pub task: Option<(TaskId, TaskState)>,
    /// The current segment.
    pub segment: u32,
    /// The input box.
    pub input: TextArea<'static>,
    /// Files attached to the next message.
    pub attachments: Vec<PathBuf>,
    /// Lines scrolled up from the bottom of the transcript (0 = follow).
    pub scroll_back: u16,
    /// Whether the terminal has focus.
    pub focused: bool,
    /// Settings for the next session.
    pub next: NextSession,
    /// Show reasoning summaries.
    pub show_thinking: bool,
    /// Animation frame for the spinner.
    pub tick: u64,
    /// Set when the user asked to leave.
    pub quit: bool,
}

/// Input box placeholder.
pub const PLACEHOLDER: &str = "Ask anything · /help for commands · Alt+Enter for a new line";

fn new_input() -> TextArea<'static> {
    let mut input = TextArea::default();
    input.set_placeholder_text(PLACEHOLDER);
    input.set_cursor_line_style(ratatui::style::Style::default());
    input.set_wrap_mode(ratatui_textarea::WrapMode::WordOrGlyph);
    input
}

/// The user's own words in a stored user message, and its attachments.
fn user_parts(message: &Message) -> (String, Vec<String>) {
    let mut text = Vec::new();
    let mut attachments = Vec::new();
    for block in &message.content {
        if let ContentBlock::Text { text: t } = block {
            if t.starts_with("<environment>") || t.starts_with("<system-note>") {
                continue;
            }
            if let Some(rest) = t.strip_prefix("<attachment path=\"") {
                attachments.push(rest.split('"').next().unwrap_or("").to_owned());
                continue;
            }
            text.push(t.as_str());
        }
    }
    (text.join("\n"), attachments)
}

/// One-line summary of a tool call's input.
#[must_use]
pub fn summarize(name: &str, input: &Value) -> String {
    let field = match name {
        "shell" | "pty_start" => input["command"].as_str().map(str::to_owned),
        "read_file" | "write_file" | "edit_file" => input["path"].as_str().map(str::to_owned),
        "glob" | "grep" => input["pattern"].as_str().map(str::to_owned),
        "ask_user" => input["question"].as_str().map(str::to_owned),
        "pty_write" => input["input"]
            .as_str()
            .map(|s| format!("{} ← {}", input["id"].as_str().unwrap_or("?"), s.trim_end())),
        "pty_read" | "pty_kill" => input["id"].as_str().map(str::to_owned),
        "apply_patch" => input["patch"].as_str().map(|p| {
            p.lines()
                .filter_map(|l| {
                    l.strip_prefix("*** Update File: ")
                        .or_else(|| l.strip_prefix("*** Add File: "))
                        .or_else(|| l.strip_prefix("*** Delete File: "))
                })
                .collect::<Vec<_>>()
                .join(", ")
        }),
        _ => None,
    };
    let text = field.unwrap_or_else(|| input.to_string());
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() > 100 {
        format!("{}…", one_line.chars().take(100).collect::<String>())
    } else {
        one_line
    }
}

/// The diff a file-changing call would make (reads the current file from disk).
#[must_use]
pub fn preview(name: &str, input: &Value, cwd: &Path) -> Vec<DiffLine> {
    let path = |p: &str| {
        let p = Path::new(p);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            cwd.join(p)
        }
    };
    match name {
        "edit_file" => {
            let (Some(old), Some(new)) = (input["old"].as_str(), input["new"].as_str()) else {
                return Vec::new();
            };
            unified(old, new, PREVIEW_LINES)
        }
        "write_file" => {
            let (Some(p), Some(content)) = (input["path"].as_str(), input["content"].as_str())
            else {
                return Vec::new();
            };
            let current = std::fs::read_to_string(path(p)).unwrap_or_default();
            unified(&current, content, PREVIEW_LINES)
        }
        "apply_patch" => input["patch"]
            .as_str()
            .map(|p| patch_lines(p, PREVIEW_LINES))
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

fn parse_effort(text: &str) -> Option<Effort> {
    match text.to_ascii_lowercase().as_str() {
        "low" => Some(Effort::Low),
        "medium" => Some(Effort::Medium),
        "high" => Some(Effort::High),
        "xhigh" => Some(Effort::Xhigh),
        "max" => Some(Effort::Max),
        _ => None,
    }
}

impl App {
    /// A fresh TUI for `attached` (its history is applied, without notifications).
    #[must_use]
    pub fn new(attached: &Attached, show_thinking: bool) -> Self {
        let mut app = Self {
            next: NextSession {
                model: attached.view.model.clone(),
                effort: attached.view.effort,
            },
            view: attached.view.clone(),
            items: Vec::new(),
            prompt: None,
            overlay: None,
            ledger: Ledger::default(),
            task: None,
            segment: 0,
            input: new_input(),
            attachments: Vec::new(),
            scroll_back: 0,
            focused: true,
            show_thinking,
            tick: 0,
            quit: false,
        };
        for envelope in &attached.history {
            app.replay(envelope);
        }
        app
    }

    /// Switches to a new session, clearing the transcript.
    pub fn attach(&mut self, attached: &Attached) {
        let keep_thinking = self.show_thinking;
        let focused = self.focused;
        *self = Self::new(attached, keep_thinking);
        self.focused = focused;
    }

    fn notice(&mut self, level: Level, text: impl Into<String>) {
        self.items.push(Item::Notice {
            level,
            text: text.into(),
        });
    }

    /// `true` while the latest task is running or waiting for approval.
    #[must_use]
    pub fn busy(&self) -> bool {
        matches!(
            self.task,
            Some((
                _,
                TaskState::Running | TaskState::AwaitingApproval | TaskState::Queued
            ))
        )
    }

    fn card(&mut self, call_id: &str) -> Option<&mut ToolCard> {
        self.items.iter_mut().rev().find_map(|i| match i {
            Item::Tool(card) if card.id == call_id => Some(card),
            _ => None,
        })
    }

    /// Applies a stored event from before attaching: same as live, without
    /// prompts, notifications or file previews (the files have changed since).
    fn replay(&mut self, envelope: &Envelope) {
        let _ = self.apply_inner(envelope, true);
        self.prompt = None;
    }

    /// Applies a live event.
    pub fn apply(&mut self, envelope: &Envelope) -> Vec<Effect> {
        self.apply_inner(envelope, false)
    }

    #[allow(clippy::too_many_lines)]
    fn apply_inner(&mut self, envelope: &Envelope, history: bool) -> Vec<Effect> {
        let mut effects = Vec::new();
        match &envelope.event {
            Event::UserInput { task, message } => {
                let (text, attachments) = user_parts(message);
                self.items.push(Item::User { text, attachments });
                self.task = Some((*task, TaskState::Running));
                self.scroll_back = 0;
            }
            Event::TextDelta { text, .. } => match self.items.last_mut() {
                Some(Item::Assistant {
                    text: t,
                    done: false,
                }) => t.push_str(text),
                _ => self.items.push(Item::Assistant {
                    text: text.clone(),
                    done: false,
                }),
            },
            Event::ThinkingDelta { text, .. } if self.show_thinking => {
                match self.items.last_mut() {
                    Some(Item::Thinking(t)) => t.push_str(text),
                    _ => self.items.push(Item::Thinking(text.clone())),
                }
            }
            Event::AssistantMessage { message, .. } => {
                let text = message.text();
                match self.items.last_mut() {
                    Some(Item::Assistant { text: t, done }) if !*done => {
                        *t = text;
                        *done = true;
                    }
                    _ if !text.is_empty() => self.items.push(Item::Assistant { text, done: true }),
                    _ => {}
                }
            }
            Event::AssistantAborted { .. } => {
                if let Some(Item::Assistant { text, done }) = self.items.last_mut()
                    && !*done
                {
                    text.push_str(" ⏹");
                    *done = true;
                }
            }
            Event::ToolCall {
                id, name, input, ..
            } => {
                if self.card(id).is_none() {
                    let todo = if name == "todo" {
                        input["items"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .map(|i| {
                                (
                                    i["status"].as_str().unwrap_or("pending").to_owned(),
                                    i["text"].as_str().unwrap_or("").to_owned(),
                                )
                            })
                            .collect()
                    } else {
                        Vec::new()
                    };
                    let preview = if history {
                        Vec::new()
                    } else {
                        preview(name, input, &self.view.cwd)
                    };
                    self.items.push(Item::Tool(ToolCard {
                        id: id.clone(),
                        name: name.clone(),
                        summary: summarize(name, input),
                        live: String::new(),
                        ok: None,
                        result: String::new(),
                        preview,
                        todo,
                    }));
                }
            }
            Event::ToolOutput { call_id, chunk } => {
                if let Some(card) = self.card(call_id) {
                    card.live.push_str(chunk);
                    if card.live.len() > LIVE_CHARS {
                        let cut = card.live.len() - LIVE_CHARS;
                        let cut = (cut..card.live.len())
                            .find(|&i| card.live.is_char_boundary(i))
                            .unwrap_or(0);
                        card.live.drain(..cut);
                    }
                }
            }
            Event::ToolFinished {
                call_id, ok, sent, ..
            } => {
                let text: String = sent
                    .iter()
                    .filter_map(|b| match b {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                let last = text
                    .lines()
                    .rev()
                    .map(str::trim)
                    .find(|l| !l.is_empty() && *l != "</untrusted>")
                    .unwrap_or("")
                    .to_owned();
                match self.card(call_id) {
                    Some(card) => {
                        card.ok = Some(*ok);
                        card.result = last;
                    }
                    // A call refused before it was announced (unknown tool).
                    None => self.notice(Level::Warning, last),
                }
            }
            Event::ApprovalRequested {
                id,
                call_id,
                reason,
                ..
            } => {
                let preview = self
                    .card(call_id)
                    .map(|c| c.preview.clone())
                    .unwrap_or_default();
                self.prompt = Some(Prompt::Approval {
                    id: *id,
                    reason: reason.clone(),
                    preview,
                });
                effects.push(Effect::Notify(format!("approval needed: {reason}")));
            }
            Event::QuestionAsked {
                id,
                question,
                options,
                multi,
                ..
            } => {
                self.prompt = Some(Prompt::Question {
                    id: *id,
                    question: question.clone(),
                    options: options.clone(),
                    multi: *multi,
                    chosen: vec![false; options.len()],
                });
                effects.push(Effect::Notify(format!("question: {question}")));
            }
            Event::ApprovalResolved { id, .. } => {
                if matches!(&self.prompt, Some(Prompt::Approval { id: p, .. } | Prompt::Reason { id: p, .. }) if p == id)
                {
                    self.prompt = None;
                }
            }
            Event::QuestionAnswered { id, .. } => {
                if matches!(&self.prompt, Some(Prompt::Question { id: p, .. }) if p == id) {
                    self.prompt = None;
                }
            }
            Event::TaskState {
                task,
                from,
                to,
                reason,
            } => {
                self.task = Some((*task, *to));
                if *from == TaskState::AwaitingApproval
                    && *to != TaskState::Running
                    && !matches!(self.prompt, Some(Prompt::Confirm { .. }))
                {
                    self.prompt = None;
                }
                match to {
                    TaskState::Completed => effects.push(Effect::Notify("task completed".into())),
                    TaskState::Failed => {
                        self.notice(Level::Error, format!("task failed: {reason}"));
                        effects.push(Effect::Notify(format!("task failed: {reason}")));
                    }
                    TaskState::BudgetExhausted => {
                        self.notice(Level::Warning, format!("stopped: {reason}"))
                    }
                    TaskState::Paused => {
                        self.notice(Level::Info, format!("task {reason} · /resume to continue"));
                    }
                    TaskState::Cancelled => self.notice(Level::Info, "task cancelled"),
                    _ => {}
                }
            }
            Event::Usage {
                task,
                usage,
                cost_micros,
            } => self.ledger.record(*task, *usage, *cost_micros),
            Event::Warning { message, .. } => self.notice(Level::Warning, message.clone()),
            Event::Error { message, hint, .. } => {
                let text = match hint {
                    Some(h) => format!("{message} ({h})"),
                    None => message.clone(),
                };
                self.notice(Level::Error, text);
            }
            Event::UndoApplied { paths, .. } => {
                self.notice(Level::Info, format!("↺ restored {}", paths.join(", ")));
            }
            Event::SegmentOpened { segment, .. } => self.segment = *segment,
            _ => {}
        }
        if history {
            effects.clear();
        }
        effects
    }

    /// Handles pasted text: file paths become attachments, anything else is typed.
    pub fn paste(&mut self, text: &str) {
        match crate::paste::attachments(text, &self.view.cwd) {
            Some(paths) => self.attachments.extend(paths),
            None => {
                self.input.insert_str(text);
            }
        }
    }

    /// Handles a key press.
    #[allow(clippy::too_many_lines)]
    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if let Some(overlay) = &mut self.overlay {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
                KeyCode::Down | KeyCode::Char('j') => {
                    overlay.scroll = overlay.scroll.saturating_add(1)
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    overlay.scroll = overlay.scroll.saturating_sub(1)
                }
                KeyCode::PageDown | KeyCode::Char(' ') => {
                    overlay.scroll = overlay.scroll.saturating_add(10)
                }
                KeyCode::PageUp => overlay.scroll = overlay.scroll.saturating_sub(10),
                KeyCode::Char('c') if ctrl => self.overlay = None,
                _ => {}
            }
            return Vec::new();
        }
        if let Some(actions) = self.prompt_key(key) {
            return actions;
        }
        match key.code {
            KeyCode::Char('c') if ctrl => {
                if self.busy() {
                    return self.interrupt();
                }
                if self.input.is_empty() && self.attachments.is_empty() {
                    self.quit = true;
                    return vec![Action::Quit];
                }
                self.input = new_input();
                self.attachments.clear();
                Vec::new()
            }
            KeyCode::Char('d') if ctrl && self.input.is_empty() => {
                self.quit = true;
                vec![Action::Quit]
            }
            KeyCode::Char('p') if ctrl => self.command(Slash::Pause),
            KeyCode::Esc if self.busy() => self.interrupt(),
            KeyCode::Esc => {
                self.attachments.clear();
                Vec::new()
            }
            KeyCode::PageUp => {
                self.scroll_back = self.scroll_back.saturating_add(10);
                Vec::new()
            }
            KeyCode::PageDown => {
                self.scroll_back = self.scroll_back.saturating_sub(10);
                Vec::new()
            }
            KeyCode::Up if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_back = self.scroll_back.saturating_add(1);
                Vec::new()
            }
            KeyCode::Down if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.scroll_back = self.scroll_back.saturating_sub(1);
                Vec::new()
            }
            KeyCode::Enter if alt || key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.insert_newline();
                Vec::new()
            }
            KeyCode::Char('j') if ctrl => {
                self.input.insert_newline();
                Vec::new()
            }
            KeyCode::Enter => self.submit(),
            _ => {
                self.input.input(key);
                Vec::new()
            }
        }
    }

    fn interrupt(&mut self) -> Vec<Action> {
        match self.task {
            Some((task, _)) => vec![Action::Send(Op::Interrupt { task })],
            None => Vec::new(),
        }
    }

    /// Keys while a prompt is open; `None` lets the key reach the input box.
    fn prompt_key(&mut self, key: KeyEvent) -> Option<Vec<Action>> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let prompt = self.prompt.take()?;
        let approve =
            |request, decision| Some(vec![Action::Send(Op::Approve { request, decision })]);
        match prompt {
            Prompt::Approval {
                id,
                reason,
                preview,
            } => {
                let decided = match key.code {
                    KeyCode::Char('o' | 'y') => approve(id, Decision::Once),
                    KeyCode::Char('s') => approve(id, Decision::ForSession),
                    KeyCode::Char('a') => approve(
                        id,
                        Decision::Always {
                            scope: RuleScope::User,
                        },
                    ),
                    KeyCode::Char('p') if !ctrl => approve(
                        id,
                        Decision::Always {
                            scope: RuleScope::Project,
                        },
                    ),
                    KeyCode::Char('d' | 'n') => {
                        self.prompt = Some(Prompt::Reason {
                            id,
                            text: String::new(),
                        });
                        return Some(Vec::new());
                    }
                    KeyCode::Esc => approve(id, Decision::Deny { reason: None }),
                    KeyCode::Char('c') if ctrl => {
                        self.prompt = Some(Prompt::Approval {
                            id,
                            reason,
                            preview,
                        });
                        return Some(self.interrupt());
                    }
                    _ => None,
                };
                if decided.is_none() {
                    self.prompt = Some(Prompt::Approval {
                        id,
                        reason,
                        preview,
                    });
                    return Some(Vec::new());
                }
                decided
            }
            Prompt::Reason { id, mut text } => match key.code {
                KeyCode::Enter => {
                    let reason = Some(text.trim().to_owned()).filter(|t| !t.is_empty());
                    approve(id, Decision::Deny { reason })
                }
                KeyCode::Esc => approve(id, Decision::Deny { reason: None }),
                KeyCode::Backspace => {
                    text.pop();
                    self.prompt = Some(Prompt::Reason { id, text });
                    Some(Vec::new())
                }
                KeyCode::Char(c) if !ctrl => {
                    text.push(c);
                    self.prompt = Some(Prompt::Reason { id, text });
                    Some(Vec::new())
                }
                _ => {
                    self.prompt = Some(Prompt::Reason { id, text });
                    Some(Vec::new())
                }
            },
            Prompt::Question {
                id,
                question,
                options,
                multi,
                mut chosen,
            } => {
                if let KeyCode::Char(c) = key.code
                    && let Some(n) = c.to_digit(10).and_then(|d| usize::try_from(d).ok())
                    && (1..=options.len()).contains(&n)
                    && self.input.is_empty()
                {
                    if !multi {
                        return Some(vec![Action::Send(Op::Answer {
                            request: id,
                            answer: options[n - 1].clone(),
                        })]);
                    }
                    chosen[n - 1] = !chosen[n - 1];
                    self.prompt = Some(Prompt::Question {
                        id,
                        question,
                        options,
                        multi,
                        chosen,
                    });
                    return Some(Vec::new());
                }
                if key.code == KeyCode::Enter {
                    let typed = self.input.lines().join("\n").trim().to_owned();
                    let answer = if typed.is_empty() {
                        options
                            .iter()
                            .zip(&chosen)
                            .filter(|(_, c)| **c)
                            .map(|(o, _)| o.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    } else {
                        typed
                    };
                    if answer.is_empty() {
                        self.prompt = Some(Prompt::Question {
                            id,
                            question,
                            options,
                            multi,
                            chosen,
                        });
                        return Some(Vec::new());
                    }
                    self.input = new_input();
                    return Some(vec![Action::Send(Op::Answer {
                        request: id,
                        answer,
                    })]);
                }
                // Anything else is typed as a free-text answer.
                self.prompt = Some(Prompt::Question {
                    id,
                    question,
                    options,
                    multi,
                    chosen,
                });
                None
            }
            Prompt::Confirm { question, then } => match key.code {
                KeyCode::Char('y') => Some(vec![*then]),
                KeyCode::Char('n') | KeyCode::Esc | KeyCode::Enter => Some(Vec::new()),
                _ => {
                    self.prompt = Some(Prompt::Confirm { question, then });
                    Some(Vec::new())
                }
            },
        }
    }

    fn submit(&mut self) -> Vec<Action> {
        let text = self.input.lines().join("\n");
        let trimmed = text.trim();
        if trimmed.is_empty() && self.attachments.is_empty() {
            return Vec::new();
        }
        if let Some(parsed) = parse(trimmed) {
            self.input = new_input();
            return match parsed {
                Ok(slash) => self.command(slash),
                Err(message) => {
                    self.notice(Level::Warning, message);
                    Vec::new()
                }
            };
        }
        if self.busy() {
            self.notice(
                Level::Warning,
                "a task is running: wait for it, press Esc to interrupt, or Ctrl-P to pause",
            );
            return Vec::new();
        }
        let mut input = Vec::new();
        if !trimmed.is_empty() {
            input.push(InputItem::Text {
                text: trimmed.to_owned(),
            });
        }
        input.extend(
            self.attachments
                .drain(..)
                .map(|path| InputItem::File { path }),
        );
        self.input = new_input();
        self.scroll_back = 0;
        vec![Action::Send(Op::Submit {
            session: self.view.id,
            input,
        })]
    }

    /// Runs the parts of a slash command that need no backend; the rest
    /// become [`Action::Command`].
    pub fn command(&mut self, slash: Slash) -> Vec<Action> {
        match slash {
            Slash::Help => {
                self.overlay = Some(crate::view::help_overlay());
                Vec::new()
            }
            Slash::Pause => match self.task {
                Some((task, TaskState::Running | TaskState::AwaitingApproval)) => {
                    self.notice(Level::Info, "pausing at the next safe point…");
                    vec![Action::Send(Op::Pause { task })]
                }
                _ => {
                    self.notice(Level::Warning, "no task is running");
                    Vec::new()
                }
            },
            Slash::Resume => match self.task {
                Some((task, TaskState::Paused)) => vec![Action::Send(Op::Resume { task })],
                _ => {
                    self.notice(Level::Warning, "no task is paused");
                    Vec::new()
                }
            },
            Slash::Undo => {
                if self.busy() {
                    self.notice(
                        Level::Warning,
                        "wait for the task (or interrupt it) before undoing",
                    );
                    return Vec::new();
                }
                vec![Action::Send(Op::Undo {
                    session: self.view.id,
                })]
            }
            Slash::Model(Some(name)) => match name.parse::<ModelRef>() {
                Ok(model) => {
                    self.next.model = model.clone();
                    self.prompt = Some(Prompt::Confirm {
                        question: format!(
                            "Start a new session with {model} now? This one stays saved. [y/N]"
                        ),
                        then: Box::new(Action::NewSession {
                            model,
                            effort: self.next.effort,
                        }),
                    });
                    Vec::new()
                }
                Err(e) => {
                    self.notice(Level::Warning, format!("{e}"));
                    Vec::new()
                }
            },
            Slash::Effort(Some(name)) => match parse_effort(&name) {
                Some(effort) => {
                    self.next.effort = Some(effort);
                    self.prompt = Some(Prompt::Confirm {
                        question: format!(
                            "Start a new session with effort {} now? This one stays saved. [y/N]",
                            name.to_ascii_lowercase()
                        ),
                        then: Box::new(Action::NewSession {
                            model: self.next.model.clone(),
                            effort: Some(effort),
                        }),
                    });
                    Vec::new()
                }
                None => {
                    self.notice(
                        Level::Warning,
                        "effort is one of low, medium, high, xhigh, max",
                    );
                    Vec::new()
                }
            },
            Slash::Clear => {
                if self.busy() {
                    self.notice(
                        Level::Warning,
                        "wait for the task (or interrupt it) before starting a new session",
                    );
                    return Vec::new();
                }
                vec![Action::NewSession {
                    model: self.next.model.clone(),
                    effort: self.next.effort,
                }]
            }
            Slash::Cost => {
                self.overlay = Some(crate::view::cost_overlay(self));
                Vec::new()
            }
            Slash::Context => {
                self.overlay = Some(crate::view::context_overlay(self));
                Vec::new()
            }
            Slash::Quit => {
                self.quit = true;
                vec![Action::Quit]
            }
            other @ (Slash::Model(None) | Slash::Effort(None) | Slash::Tasks | Slash::Diff) => {
                vec![Action::Command(other)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::BlobOrText;
    use harness_types::{Risk, Seq, SessionId, Usage};
    use tokio::sync::broadcast;

    fn attached(dir: &Path) -> Attached {
        let (_tx, rx) = broadcast::channel(4);
        Attached {
            view: SessionView {
                id: SessionId::new(),
                persona: "default".into(),
                model: "openai/gpt-6-luna".parse().unwrap(),
                effort: Some(Effort::High),
                profile: "auto".into(),
                context_limit: 250_000,
                priced: false,
                cwd: dir.to_path_buf(),
            },
            events: rx,
            history: Vec::new(),
        }
    }

    fn env(event: Event) -> Envelope {
        Envelope {
            seq: Seq(1),
            ts: 0,
            session: SessionId::new(),
            event,
        }
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.key(key(KeyCode::Char(c)));
        }
    }

    fn running(app: &mut App) -> TaskId {
        let task = TaskId::new();
        app.apply(&env(Event::UserInput {
            task,
            message: Message::user_text("go"),
        }));
        task
    }

    #[test]
    fn typing_and_enter_submit_a_prompt_with_attachments() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("spec.md"), "x").unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        type_text(&mut app, "summarise this");
        app.paste("spec.md");
        assert_eq!(app.attachments, [dir.path().join("spec.md")]);
        let actions = app.key(key(KeyCode::Enter));
        let [Action::Send(Op::Submit { input, .. })] = actions.as_slice() else {
            panic!("{actions:?}")
        };
        assert_eq!(
            input[0],
            InputItem::Text {
                text: "summarise this".into()
            }
        );
        assert_eq!(
            input[1],
            InputItem::File {
                path: dir.path().join("spec.md")
            }
        );
        assert!(app.input.is_empty() && app.attachments.is_empty());
    }

    #[test]
    fn pasted_text_is_typed_and_alt_enter_adds_a_line() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        app.paste("hello world");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut app, "two");
        assert_eq!(app.input.lines(), ["hello world", "two"]);
    }

    #[test]
    fn streaming_text_becomes_the_final_reply() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.apply(&env(Event::TextDelta {
            task,
            text: "Hel".into(),
        }));
        app.apply(&env(Event::TextDelta {
            task,
            text: "lo".into(),
        }));
        assert_eq!(
            app.items[1],
            Item::Assistant {
                text: "Hello".into(),
                done: false
            }
        );
        app.apply(&env(Event::AssistantMessage {
            task,
            message: Message::new(
                harness_types::Role::Assistant,
                vec![ContentBlock::text("Hello!")],
            ),
        }));
        assert_eq!(
            app.items[1],
            Item::Assistant {
                text: "Hello!".into(),
                done: true
            }
        );
    }

    #[test]
    fn tool_cards_collect_live_output_and_results() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.apply(&env(Event::ToolCall {
            task,
            id: "c1".into(),
            name: "shell".into(),
            input: serde_json::json!({"command": "cargo test"}),
            risk: Risk::Exec {
                command: "cargo test".into(),
            },
        }));
        app.apply(&env(Event::ToolOutput {
            call_id: "c1".into(),
            chunk: "running 3 tests\n".into(),
        }));
        app.apply(&env(Event::ToolFinished {
            call_id: "c1".into(),
            ok: true,
            full: BlobOrText::Text {
                text: String::new(),
            },
            sent: vec![ContentBlock::text("ok\n[exit code 0]")],
        }));
        let Item::Tool(card) = &app.items[1] else {
            panic!()
        };
        assert_eq!(
            (card.summary.as_str(), card.ok, card.result.as_str()),
            ("cargo test", Some(true), "[exit code 0]")
        );
        assert_eq!(card.live, "running 3 tests\n");
    }

    #[test]
    fn edit_calls_get_a_diff_preview_that_the_approval_shows() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.apply(&env(Event::ToolCall {
            task,
            id: "c1".into(),
            name: "edit_file".into(),
            input: serde_json::json!({"path": "a.rs", "old": "x < y", "new": "x <= y"}),
            risk: Risk::Write { paths: vec![] },
        }));
        let effects = app.apply(&env(Event::ApprovalRequested {
            id: ApprovalId::new(),
            task,
            call_id: "c1".into(),
            reason: "write a.rs".into(),
        }));
        assert_eq!(
            effects,
            [Effect::Notify("approval needed: write a.rs".into())]
        );
        let Some(Prompt::Approval { preview, .. }) = &app.prompt else {
            panic!()
        };
        assert!(preview.contains(&DiffLine::Added("x <= y".into())));
    }

    #[test]
    fn approval_keys_send_decisions_and_d_asks_for_a_reason() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let id = ApprovalId::new();
        let ask = |app: &mut App| {
            app.prompt = Some(Prompt::Approval {
                id,
                reason: "r".into(),
                preview: vec![],
            })
        };
        for (k, want) in [
            ('o', Decision::Once),
            ('s', Decision::ForSession),
            (
                'a',
                Decision::Always {
                    scope: RuleScope::User,
                },
            ),
            (
                'p',
                Decision::Always {
                    scope: RuleScope::Project,
                },
            ),
        ] {
            ask(&mut app);
            assert_eq!(
                app.key(key(KeyCode::Char(k))),
                [Action::Send(Op::Approve {
                    request: id,
                    decision: want
                })]
            );
        }
        ask(&mut app);
        assert!(app.key(key(KeyCode::Char('d'))).is_empty());
        type_text(&mut app, "use make");
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::Send(Op::Approve {
                request: id,
                decision: Decision::Deny {
                    reason: Some("use make".into())
                }
            })]
        );
        ask(&mut app);
        assert!(
            app.key(key(KeyCode::Char('x'))).is_empty(),
            "other keys keep the prompt"
        );
        assert!(app.prompt.is_some());
    }

    #[test]
    fn questions_take_a_number_or_typed_words() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let id = QuestionId::new();
        let question = |multi| Prompt::Question {
            id,
            question: "Which?".into(),
            options: vec!["red".into(), "blue".into()],
            multi,
            chosen: vec![false, false],
        };
        app.prompt = Some(question(false));
        assert_eq!(
            app.key(key(KeyCode::Char('2'))),
            [Action::Send(Op::Answer {
                request: id,
                answer: "blue".into()
            })]
        );
        app.prompt = Some(question(true));
        app.key(key(KeyCode::Char('1')));
        app.key(key(KeyCode::Char('2')));
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::Send(Op::Answer {
                request: id,
                answer: "red, blue".into()
            })]
        );
        app.prompt = Some(question(false));
        type_text(&mut app, "green");
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::Send(Op::Answer {
                request: id,
                answer: "green".into()
            })]
        );
    }

    #[test]
    fn ctrl_c_and_esc_interrupt_a_running_task_otherwise_quit() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        assert_eq!(app.key(ctrl('c')), [Action::Send(Op::Interrupt { task })]);
        assert_eq!(
            app.key(key(KeyCode::Esc)),
            [Action::Send(Op::Interrupt { task })]
        );
        app.apply(&env(Event::TaskState {
            task,
            from: TaskState::Running,
            to: TaskState::Completed,
            reason: "done".into(),
        }));
        type_text(&mut app, "draft");
        assert!(app.key(ctrl('c')).is_empty(), "first clears the input");
        assert_eq!(app.key(ctrl('c')), [Action::Quit]);
    }

    #[test]
    fn prompts_are_refused_while_busy_and_slash_commands_parse() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        type_text(&mut app, "more");
        assert!(app.key(key(KeyCode::Enter)).is_empty());
        assert!(matches!(
            app.items.last(),
            Some(Item::Notice {
                level: Level::Warning,
                ..
            })
        ));
        app.input = new_input();
        type_text(&mut app, "/pause");
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::Send(Op::Pause { task })]
        );
        type_text(&mut app, "/tasks");
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::Command(Slash::Tasks)]
        );
        type_text(&mut app, "/nope");
        assert!(app.key(key(KeyCode::Enter)).is_empty());
    }

    #[test]
    fn model_and_effort_offer_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        app.command(Slash::Model(Some("anthropic/claude-opus-5-5".into())));
        assert!(matches!(app.prompt, Some(Prompt::Confirm { .. })));
        let model: ModelRef = "anthropic/claude-opus-5-5".parse().unwrap();
        assert_eq!(
            app.key(key(KeyCode::Char('y'))),
            [Action::NewSession {
                model: model.clone(),
                effort: Some(Effort::High)
            }]
        );
        app.command(Slash::Effort(Some("low".into())));
        assert!(
            app.key(key(KeyCode::Char('n'))).is_empty(),
            "no keeps the session"
        );
        assert_eq!(
            app.next,
            NextSession {
                model,
                effort: Some(Effort::Low)
            }
        );
    }

    #[test]
    fn task_states_update_the_status_and_prompts() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.prompt = Some(Prompt::Approval {
            id: ApprovalId::new(),
            reason: "r".into(),
            preview: vec![],
        });
        app.apply(&env(Event::TaskState {
            task,
            from: TaskState::AwaitingApproval,
            to: TaskState::Paused,
            reason: "paused".into(),
        }));
        assert!(app.prompt.is_none(), "a pause drops the prompt");
        assert_eq!(app.task, Some((task, TaskState::Paused)));
        assert_eq!(
            app.command(Slash::Resume),
            [Action::Send(Op::Resume { task })]
        );
        app.apply(&env(Event::Usage {
            task,
            usage: Usage {
                input: 10,
                output: 5,
                ..Usage::default()
            },
            cost_micros: 7,
        }));
        assert_eq!(app.ledger.session().cost_micros, 7);
    }

    #[test]
    fn history_is_shown_without_prompts_or_notifications() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = attached(dir.path());
        let task = TaskId::new();
        a.history = vec![
            env(Event::UserInput {
                task,
                message: Message::new(
                    harness_types::Role::User,
                    vec![
                        ContentBlock::text("<environment>e</environment>"),
                        ContentBlock::text(
                            "<attachment path=\"/p/a.png\" bytes=\"3\">…</attachment>",
                        ),
                        ContentBlock::text("look"),
                    ],
                ),
            }),
            env(Event::ApprovalRequested {
                id: ApprovalId::new(),
                task,
                call_id: "c".into(),
                reason: "r".into(),
            }),
        ];
        let app = App::new(&a, false);
        assert_eq!(
            app.items[0],
            Item::User {
                text: "look".into(),
                attachments: vec!["/p/a.png".into()]
            }
        );
        assert!(app.prompt.is_none());
    }

    #[test]
    fn todo_calls_carry_their_checklist() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.apply(&env(Event::ToolCall {
            task,
            id: "t".into(),
            name: "todo".into(),
            input: serde_json::json!({"items": [{"text": "a", "status": "completed"}, {"text": "b", "status": "pending"}]}),
            risk: Risk::ReadOnly { paths: vec![] },
        }));
        let Item::Tool(card) = &app.items[1] else {
            panic!()
        };
        assert_eq!(
            card.todo,
            [
                ("completed".to_owned(), "a".to_owned()),
                ("pending".to_owned(), "b".to_owned())
            ]
        );
    }
}
