//! [`App`]: the TUI's state and every transition, kept free of I/O so it can be
//! tested directly. Events from the session go in through [`App::apply`];
//! keys and pastes through [`App::key`] and [`App::paste`]. Both return what the
//! run loop must do ([`Action`], [`Effect`]).

use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind};
use harness_ports::Effort;
use harness_protocol::{Decision, Envelope, Event, InputItem, Op, RuleScope, TaskState};
use harness_types::{
    ApprovalId, ContentBlock, Message, Mode, ModelRef, QuestionId, SessionId, TaskId,
};
use ratatui::text::Line;
use ratatui_textarea::TextArea;
use serde_json::Value;

use crate::backend::{Attached, SessionSummary, SessionView};
use crate::diff::{DiffLine, patch_lines, unified};
use crate::ledger::Ledger;
use crate::slash::{Slash, parse};

/// Lines of a diff preview kept on a tool card and in the approval panel.
const PREVIEW_LINES: usize = 40;
/// Live output kept per tool card (the tail).
const LIVE_CHARS: usize = 4_000;
/// Result lines kept on a finished command's card.
const COMMAND_RESULT_LINES: usize = 5;

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
    /// The end of the result: the last lines of a command's output, or the
    /// last line for other tools.
    pub result: Vec<String>,
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
        /// The rendered Markdown, made once when the reply is done (a streaming
        /// reply is rendered on each frame), so long sessions stay fast.
        rendered: Vec<Line<'static>>,
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

impl Item {
    /// A finished reply, rendered.
    #[must_use]
    pub fn assistant_done(text: impl Into<String>) -> Self {
        let text = text.into();
        Self::Assistant {
            rendered: crate::markdown::render(&text),
            text,
            done: true,
        }
    }
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
        /// The model's one-line description of the call (ADR-043), shown
        /// above `reason`, never instead of it.
        summary: Option<String>,
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
    /// Lines that can be picked (`/sessions`, `/model`); empty for plain panels.
    pub picks: Vec<Pick>,
    /// The highlighted pick.
    pub selected: usize,
}

/// A line of an [`Overlay`] that does something when picked.
#[derive(Debug, Clone, PartialEq)]
pub struct Pick {
    /// Index into [`Overlay::lines`].
    pub line: usize,
    /// What picking it does.
    pub action: Action,
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
    /// Switch to a stored session.
    OpenSession(SessionId),
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
    /// Earlier prompts, oldest first.
    pub prompts: Vec<String>,
    /// Which earlier prompt the input shows (`None`: the user's own draft).
    recalled: Option<usize>,
    /// What was typed before ↑ started recalling.
    draft: String,
}

/// Earlier prompts kept for ↑.
pub const PROMPT_HISTORY: usize = 500;
/// Lines one notch of the mouse wheel scrolls.
pub const WHEEL_LINES: u16 = 3;

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
        // The plan itself is shown in full below the card.
        "propose_plan" => Some("a plan for your approval".to_owned()),
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
            prompts: attached.prompts.clone(),
            recalled: None,
            draft: String::new(),
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

    /// Shows `text` in the input box with the cursor at its end.
    fn set_input(&mut self, text: &str) {
        self.input = new_input();
        self.input.insert_str(text);
    }

    /// ↑ on the first line of the input: the previous prompt.
    fn recall_older(&mut self) -> bool {
        let next = match self.recalled {
            None if self.prompts.is_empty() => return false,
            None => {
                self.draft = self.input.lines().join("\n");
                self.prompts.len() - 1
            }
            Some(i) => i.saturating_sub(1),
        };
        self.recalled = Some(next);
        let text = self.prompts[next].clone();
        self.set_input(&text);
        true
    }

    /// ↓ on the last line of the input: the next prompt, then the draft.
    fn recall_newer(&mut self) -> bool {
        let Some(i) = self.recalled else {
            return false;
        };
        if i + 1 < self.prompts.len() {
            self.recalled = Some(i + 1);
            let text = self.prompts[i + 1].clone();
            self.set_input(&text);
        } else {
            self.recalled = None;
            let draft = std::mem::take(&mut self.draft);
            self.set_input(&draft);
        }
        true
    }

    /// Adds a sent prompt to the history (not twice in a row).
    fn remember(&mut self, prompt: &str) {
        self.recalled = None;
        self.draft.clear();
        if self.prompts.last().map(String::as_str) != Some(prompt) {
            self.prompts.push(prompt.to_owned());
        }
        if self.prompts.len() > PROMPT_HISTORY {
            let extra = self.prompts.len() - PROMPT_HISTORY;
            self.prompts.drain(..extra);
        }
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
                    ..
                }) => t.push_str(text),
                _ => self.items.push(Item::Assistant {
                    text: text.clone(),
                    done: false,
                    rendered: Vec::new(),
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
                    Some(last @ Item::Assistant { done: false, .. }) => {
                        *last = Item::assistant_done(text);
                    }
                    _ if !text.is_empty() => self.items.push(Item::assistant_done(text)),
                    _ => {}
                }
            }
            Event::AssistantAborted { .. } => {
                if let Some(last) = self.items.last_mut()
                    && let Item::Assistant {
                        text, done: false, ..
                    } = last
                {
                    let text = format!("{text} ⏹");
                    *last = Item::assistant_done(text);
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
                        result: Vec::new(),
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
                let mut tail: Vec<String> = text
                    .lines()
                    .rev()
                    .map(str::trim_end)
                    .filter(|l| {
                        !l.trim().is_empty()
                            && *l != "</untrusted>"
                            && !l.starts_with("<untrusted source=")
                    })
                    .take(COMMAND_RESULT_LINES)
                    .map(str::to_owned)
                    .collect();
                tail.reverse();
                match self.card(call_id) {
                    Some(card) => {
                        let command =
                            matches!(card.name.as_str(), "shell" | "pty_read" | "pty_kill");
                        if !command && tail.len() > 1 {
                            tail.drain(..tail.len() - 1);
                        }
                        card.ok = Some(*ok);
                        card.result = tail;
                    }
                    // A call refused before it was announced (unknown tool).
                    None => self.notice(Level::Warning, tail.pop().unwrap_or_default()),
                }
            }
            Event::ApprovalRequested {
                id,
                call_id,
                reason,
                summary,
                ..
            } => {
                let preview = self
                    .card(call_id)
                    .map(|c| c.preview.clone())
                    .unwrap_or_default();
                self.prompt = Some(Prompt::Approval {
                    id: *id,
                    reason: reason.clone(),
                    summary: summary.clone(),
                    preview,
                });
                let what = summary.as_deref().unwrap_or(reason);
                effects.push(Effect::Notify(format!("approval needed: {what}")));
            }
            Event::QuestionAsked {
                id,
                call_id,
                question,
                options,
                multi,
                ..
            } => {
                // A plan is long Markdown: it goes in the transcript, rendered,
                // and the panel only asks for the decision.
                let is_plan = self.card(call_id).is_some_and(|c| c.name == "propose_plan");
                let question = if is_plan {
                    self.notice(Level::Info, "proposed plan:");
                    self.items.push(Item::assistant_done(question.clone()));
                    "Approve this plan? (or type what to change)".to_owned()
                } else {
                    question.clone()
                };
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
            Event::SessionOpened {
                mode: Some(mode), ..
            } => self.view.mode = *mode,
            Event::ModeChanged { to, .. } => {
                self.view.mode = *to;
                self.notice(Level::Info, format!("mode: {to} · {}", to.describe()));
            }
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

    /// Handles a mouse event (only the wheel does something): scrolls the open
    /// panel, else the transcript.
    pub fn mouse(&mut self, event: MouseEvent) {
        let up = match event.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return,
        };
        let scroll = match &mut self.overlay {
            Some(overlay) => &mut overlay.scroll,
            // The transcript counts lines back from the bottom: up means more.
            None => {
                self.scroll_back = if up {
                    self.scroll_back.saturating_add(WHEEL_LINES)
                } else {
                    self.scroll_back.saturating_sub(WHEEL_LINES)
                };
                return;
            }
        };
        *scroll = if up {
            scroll.saturating_sub(WHEEL_LINES)
        } else {
            scroll.saturating_add(WHEEL_LINES)
        };
    }

    /// Handles a key press.
    #[allow(clippy::too_many_lines)]
    pub fn key(&mut self, key: KeyEvent) -> Vec<Action> {
        if key.kind == KeyEventKind::Release {
            return Vec::new();
        }
        if self.overlay.is_some() {
            return self.overlay_key(key);
        }
        if let Some(actions) = self.prompt_key(key) {
            return actions;
        }
        self.input_key(key)
    }

    /// Keys while an overlay is open: scrolling, and choosing for pickers.
    fn overlay_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let Some(overlay) = &mut self.overlay else {
            return Vec::new();
        };
        if !overlay.picks.is_empty() {
            let last = overlay.picks.len() - 1;
            let chosen = match key.code {
                KeyCode::Up | KeyCode::Char('k') => {
                    overlay.selected = overlay.selected.saturating_sub(1);
                    let line = u16::try_from(overlay.picks[overlay.selected].line).unwrap_or(0);
                    overlay.scroll = overlay.scroll.min(line);
                    return Vec::new();
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    overlay.selected = (overlay.selected + 1).min(last);
                    return Vec::new();
                }
                KeyCode::Enter => Some(overlay.selected),
                KeyCode::Char(c @ '1'..='9') => c
                    .to_digit(10)
                    .and_then(|d| usize::try_from(d).ok())
                    .map(|d| d - 1)
                    .filter(|&i| i <= last),
                _ => None,
            };
            if let Some(i) = chosen {
                let action = overlay.picks[i].action.clone();
                self.overlay = None;
                return self.pick(action);
            }
        }
        let Some(overlay) = &mut self.overlay else {
            return Vec::new();
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.overlay = None,
            KeyCode::Down | KeyCode::Char('j') => overlay.scroll = overlay.scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => overlay.scroll = overlay.scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => {
                overlay.scroll = overlay.scroll.saturating_add(10)
            }
            KeyCode::PageUp => overlay.scroll = overlay.scroll.saturating_sub(10),
            KeyCode::Char('c') if ctrl => self.overlay = None,
            _ => {}
        }
        Vec::new()
    }

    /// What picking an overlay line does; switching sessions is refused
    /// while a task runs.
    fn pick(&mut self, action: Action) -> Vec<Action> {
        match action {
            Action::OpenSession(id) if id == self.view.id => {
                self.notice(Level::Info, "you are in this session already");
                Vec::new()
            }
            Action::OpenSession(_) if self.busy() => {
                self.notice(
                    Level::Warning,
                    "wait for the task (or interrupt it) before switching sessions",
                );
                Vec::new()
            }
            other => vec![other],
        }
    }

    /// On a new session: mention the most recent other one, and how to get back to it.
    pub fn hint_last_session(&mut self, sessions: &[SessionSummary], now_ms: i64) {
        let Some(last) = sessions.iter().find(|s| s.id != self.view.id) else {
            return;
        };
        let title = last.title.as_deref().unwrap_or("(no prompt yet)");
        self.notice(
            Level::Info,
            format!(
                "last session: “{}” · {} · /sessions to switch, or `harness -c` to start in it",
                crate::view::shorten(title, 60),
                crate::view::ago(now_ms, last.last_active)
            ),
        );
    }

    /// Keys for the input box (no overlay or prompt took them).
    fn input_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
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
            KeyCode::BackTab => {
                let next = self.view.mode.next(self.view.full_access);
                self.switch_mode(next)
            }
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
            KeyCode::Up if self.input.cursor().0 == 0 && self.recall_older() => Vec::new(),
            KeyCode::Down
                if self.input.cursor().0 + 1 >= self.input.lines().len() && self.recall_newer() =>
            {
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

    /// Asks the runtime to switch modes; the header changes when the
    /// `ModeChanged` event arrives, so it never shows a mode not in force.
    fn switch_mode(&mut self, mode: Mode) -> Vec<Action> {
        vec![Action::Send(Op::SetMode {
            session: self.view.id,
            mode,
        })]
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
                summary,
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
                            summary,
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
                        summary,
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
            let typed = trimmed.to_owned();
            self.remember(&typed);
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
        if !trimmed.is_empty() {
            let prompt = trimmed.to_owned();
            self.remember(&prompt);
        }
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
            Slash::Mode(None) => {
                self.overlay = Some(crate::view::modes_overlay(self));
                Vec::new()
            }
            Slash::Mode(Some(name)) => match name.parse::<Mode>() {
                Ok(Mode::Full) if !self.view.full_access => {
                    self.notice(
                        Level::Warning,
                        "full mode needs harness to be started with --dangerously-full-access",
                    );
                    Vec::new()
                }
                Ok(mode) => self.switch_mode(mode),
                Err(e) => {
                    self.notice(Level::Warning, e.to_string());
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
            other @ (Slash::Model(None)
            | Slash::Effort(None)
            | Slash::Tasks
            | Slash::Diff
            | Slash::Sessions) => {
                vec![Action::Command(other)]
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::SessionSummary;
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
                mode: Mode::Auto,
                full_access: false,
                context_limit: 250_000,
                priced: false,
                cwd: dir.to_path_buf(),
            },
            events: rx,
            history: Vec::new(),
            prompts: Vec::new(),
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
                done: false,
                rendered: Vec::new(),
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
                done: true,
                rendered: crate::markdown::render("Hello!"),
            },
            "a finished reply is rendered once"
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
            sent: vec![ContentBlock::text(
                "<untrusted source=\"shell\">\nrunning 3 tests\ntest a ... ok\ntest b ... ok\n\ntest c ... ok\ntest result: ok. 3 passed\n[exit code 0]\n</untrusted>",
            )],
        }));
        let Item::Tool(card) = &app.items[1] else {
            panic!()
        };
        assert_eq!((card.summary.as_str(), card.ok), ("cargo test", Some(true)));
        assert_eq!(
            card.result,
            [
                "test a ... ok",
                "test b ... ok",
                "test c ... ok",
                "test result: ok. 3 passed",
                "[exit code 0]"
            ],
            "a finished command keeps its last lines"
        );
        assert_eq!(card.live, "running 3 tests\n");

        // Other tools keep one line.
        app.apply(&env(Event::ToolCall {
            task,
            id: "c2".into(),
            name: "read_file".into(),
            input: serde_json::json!({"path": "a.rs"}),
            risk: Risk::ReadOnly { paths: vec![] },
        }));
        app.apply(&env(Event::ToolFinished {
            call_id: "c2".into(),
            ok: true,
            full: BlobOrText::Text {
                text: String::new(),
            },
            sent: vec![ContentBlock::text("1 fn a() {}\n2 fn b() {}")],
        }));
        let Item::Tool(card) = &app.items[2] else {
            panic!()
        };
        assert_eq!(card.result, ["2 fn b() {}"]);
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
            summary: None,
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
    fn approvals_keep_the_description_and_notify_with_it() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        let effects = app.apply(&env(Event::ApprovalRequested {
            id: ApprovalId::new(),
            task,
            call_id: "c1".into(),
            reason: "run `cargo test -p x`".into(),
            summary: Some("Run the tests".into()),
        }));
        assert_eq!(
            effects,
            [Effect::Notify("approval needed: Run the tests".into())]
        );
        let Some(Prompt::Approval {
            summary, reason, ..
        }) = &app.prompt
        else {
            panic!()
        };
        assert_eq!(summary.as_deref(), Some("Run the tests"));
        assert_eq!(reason, "run `cargo test -p x`");
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
                summary: None,
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
            summary: None,
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
                summary: None,
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

    fn summary(title: &str, minutes_ago: i64) -> SessionSummary {
        SessionSummary {
            id: SessionId::new(),
            title: Some(title.into()),
            last_active: NOW - minutes_ago * 60_000,
            state: Some(TaskState::Completed),
        }
    }

    const NOW: i64 = 1_800_000_000_000;

    #[test]
    fn the_sessions_picker_opens_a_session_by_arrows_enter_or_number() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let mut current = summary("this one", 0);
        current.id = app.view.id;
        let list = vec![current, summary("login bug", 30), summary("readme", 600)];
        assert_eq!(
            app.command(Slash::Sessions),
            [Action::Command(Slash::Sessions)]
        );

        app.overlay = Some(crate::view::sessions_overlay(&app, &list, NOW));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Down));
        app.key(key(KeyCode::Up));
        assert_eq!(
            app.key(key(KeyCode::Enter)),
            [Action::OpenSession(list[1].id)]
        );
        assert!(app.overlay.is_none(), "picking closes the list");

        app.overlay = Some(crate::view::sessions_overlay(&app, &list, NOW));
        assert_eq!(
            app.key(key(KeyCode::Char('3'))),
            [Action::OpenSession(list[2].id)]
        );

        // The current session, or any while a task runs: nothing to do.
        app.overlay = Some(crate::view::sessions_overlay(&app, &list, NOW));
        assert!(app.key(key(KeyCode::Char('1'))).is_empty());
        assert!(
            matches!(app.items.last(), Some(Item::Notice { text, .. }) if text.contains("already"))
        );
        running(&mut app);
        app.overlay = Some(crate::view::sessions_overlay(&app, &list, NOW));
        assert!(app.key(key(KeyCode::Char('2'))).is_empty());
        assert!(matches!(
            app.items.last(),
            Some(Item::Notice {
                level: Level::Warning,
                ..
            })
        ));
    }

    #[test]
    fn the_model_list_picks_a_model_for_a_new_session() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let models: Vec<ModelRef> = vec![
            "openai/gpt-6-luna".parse().unwrap(),
            "anthropic/claude-opus-5-5".parse().unwrap(),
        ];
        app.overlay = Some(crate::view::models_overlay(&app, &models));
        assert_eq!(
            app.key(key(KeyCode::Char('2'))),
            [Action::Command(Slash::Model(Some(
                "anthropic/claude-opus-5-5".into()
            )))]
        );
    }

    #[test]
    fn shift_tab_asks_for_the_next_mode_and_skips_full_unless_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let id = app.view.id;
        let set = |mode| [Action::Send(Op::SetMode { session: id, mode })];
        assert_eq!(app.key(key(KeyCode::BackTab)), set(Mode::Plan));
        // The header follows the runtime, not the key press.
        assert_eq!(app.view.mode, Mode::Auto);
        app.view.mode = Mode::Plan;
        assert_eq!(app.key(key(KeyCode::BackTab)), set(Mode::ReadOnly));
        app.view.full_access = true;
        assert_eq!(app.key(key(KeyCode::BackTab)), set(Mode::Full));
        // Not while a question waits for an answer.
        app.prompt = Some(Prompt::Approval {
            id: ApprovalId::new(),
            reason: "r".into(),
            summary: None,
            preview: vec![],
        });
        assert!(app.key(key(KeyCode::BackTab)).is_empty());
    }

    #[test]
    fn mode_changes_update_the_header_and_history_restores_the_mode() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        app.apply(&env(Event::ModeChanged {
            from: Mode::Auto,
            to: Mode::Plan,
            note: "n".into(),
        }));
        assert_eq!(app.view.mode, Mode::Plan);
        assert!(
            matches!(app.items.last(), Some(Item::Notice { text, .. }) if text.starts_with("mode: plan"))
        );

        let mut stored = attached(dir.path());
        stored.history = vec![env(Event::SessionOpened {
            persona: "default".into(),
            segment: 0,
            mode: Some(Mode::ReadOnly),
        })];
        assert_eq!(App::new(&stored, false).view.mode, Mode::ReadOnly);
    }

    #[test]
    fn the_mode_command_switches_lists_or_refuses() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let id = app.view.id;
        assert_eq!(
            app.command(Slash::Mode(Some("plan".into()))),
            [Action::Send(Op::SetMode {
                session: id,
                mode: Mode::Plan
            })]
        );
        assert!(app.command(Slash::Mode(Some("full".into()))).is_empty());
        assert!(
            matches!(app.items.last(), Some(Item::Notice { text, .. }) if text.contains("--dangerously-full-access"))
        );
        assert!(app.command(Slash::Mode(Some("yolo".into()))).is_empty());
        assert!(app.command(Slash::Mode(None)).is_empty());
        let overlay = app.overlay.clone().unwrap();
        assert_eq!(overlay.picks.len(), 3, "full is not offered");
        assert_eq!(
            app.key(key(KeyCode::Char('1'))),
            [Action::Command(Slash::Mode(Some("read-only".into())))]
        );
    }

    #[test]
    fn plan_cards_do_not_repeat_the_plan() {
        assert_eq!(
            summarize("propose_plan", &serde_json::json!({"plan": "## Plan"})),
            "a plan for your approval"
        );
    }

    #[test]
    fn a_proposed_plan_goes_in_the_transcript_and_the_panel_asks_to_approve() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        let task = running(&mut app);
        app.apply(&env(Event::ToolCall {
            task,
            id: "p1".into(),
            name: "propose_plan".into(),
            input: serde_json::json!({"plan": "## Plan\n1. Fix it"}),
            risk: Risk::ReadOnly { paths: vec![] },
        }));
        app.apply(&env(Event::QuestionAsked {
            id: QuestionId::new(),
            task,
            call_id: "p1".into(),
            question: "## Plan\n1. Fix it".into(),
            options: vec!["Yes, and auto-accept edits".into()],
            multi: false,
        }));
        assert!(
            matches!(app.items.last(), Some(Item::Assistant { text, .. }) if text.starts_with("## Plan"))
        );
        let Some(Prompt::Question { question, .. }) = &app.prompt else {
            panic!("{:?}", app.prompt)
        };
        assert!(question.starts_with("Approve this plan?"), "{question}");
    }

    #[test]
    fn a_new_session_mentions_the_last_one() {
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        app.hint_last_session(&[summary("fix the login test", 120)], NOW);
        let Some(Item::Notice { text, .. }) = app.items.last() else {
            panic!("{:?}", app.items)
        };
        assert!(
            text.contains("fix the login test")
                && text.contains("2 h ago")
                && text.contains("/sessions")
                && text.contains("harness -c"),
            "{text}"
        );
        let mut alone = App::new(&attached(dir.path()), false);
        let mut me = summary("me", 0);
        me.id = alone.view.id;
        alone.hint_last_session(&[me], NOW);
        assert!(alone.items.is_empty(), "only the current session: no hint");
    }

    fn input_text(app: &App) -> String {
        app.input.lines().join("\n")
    }

    #[test]
    fn up_and_down_walk_through_earlier_prompts_and_back_to_the_draft() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = attached(dir.path());
        a.prompts = vec!["first".into(), "second".into()];
        let mut app = App::new(&a, false);

        type_text(&mut app, "dra");
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "second");
        app.key(key(KeyCode::Up));
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "first", "stops at the oldest");
        app.key(key(KeyCode::Down));
        assert_eq!(input_text(&app), "second");
        app.key(key(KeyCode::Down));
        assert_eq!(input_text(&app), "dra", "back to what was being typed");

        // A sent prompt joins the history (once, even if sent twice).
        app.input = new_input();
        type_text(&mut app, "third");
        app.key(key(KeyCode::Enter));
        type_text(&mut app, "third");
        app.key(key(KeyCode::Enter));
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "third");
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "second");
    }

    #[test]
    fn up_moves_within_a_multi_line_prompt_before_recalling_history() {
        let dir = tempfile::tempdir().unwrap();
        let mut a = attached(dir.path());
        a.prompts = vec!["old".into()];
        let mut app = App::new(&a, false);
        type_text(&mut app, "one");
        app.key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT));
        type_text(&mut app, "two");
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "one\ntwo", "first Up moves to line one");
        app.key(key(KeyCode::Up));
        assert_eq!(input_text(&app), "old");
    }

    #[test]
    fn the_mouse_wheel_scrolls_the_transcript_or_the_open_panel() {
        use crossterm::event::{MouseEvent, MouseEventKind};
        let wheel = |kind| MouseEvent {
            kind,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let dir = tempfile::tempdir().unwrap();
        let mut app = App::new(&attached(dir.path()), false);
        app.mouse(wheel(MouseEventKind::ScrollUp));
        app.mouse(wheel(MouseEventKind::ScrollUp));
        assert_eq!(app.scroll_back, 2 * WHEEL_LINES);
        app.mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.scroll_back, WHEEL_LINES);
        app.mouse(wheel(MouseEventKind::Moved));
        assert_eq!(
            app.scroll_back, WHEEL_LINES,
            "other mouse events do nothing"
        );

        app.overlay = Some(crate::view::help_overlay());
        app.mouse(wheel(MouseEventKind::ScrollDown));
        assert_eq!(app.overlay.as_ref().unwrap().scroll, WHEEL_LINES);
        assert_eq!(app.scroll_back, WHEEL_LINES, "the transcript stays put");
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
