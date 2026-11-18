//! Renders logged events as text with an event number on every line, for the
//! compaction model and for `recall` (Tech spec 3.13).
//!
//! Tool results appear in the capped form the model saw (`ToolFinished.sent`),
//! never the full output, so a summary describes what the model actually knew.

use std::collections::{BTreeMap, HashMap};

use harness_protocol::Event;
use harness_types::{ContentBlock, Message, Seq};

use crate::estimate_tokens;

/// The text of one rendered event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventText {
    /// The event's content without prefixes: pinned quotes must be exact
    /// substrings of it (I5).
    pub raw: String,
    /// The rendered lines, each starting with `#seq`.
    pub lines: Vec<String>,
    /// Estimated tokens of the rendered lines.
    pub tokens: u64,
    /// `true` when a new step starts here (user input, or a reply after tool
    /// results): automatic anchors are cut before such events.
    pub step_start: bool,
}

/// A run of events rendered for compaction or recall.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Rendered {
    /// Rendered events by sequence number (events with no content are absent).
    pub events: BTreeMap<Seq, EventText>,
    /// First and last sequence numbers of the run, rendered or not.
    pub span: Option<(Seq, Seq)>,
}

impl Rendered {
    /// All lines, in order.
    #[must_use]
    pub fn text(&self) -> String {
        self.events
            .values()
            .flat_map(|e| e.lines.iter().map(String::as_str))
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Estimated tokens of the events from `from` to `to`, both included.
    #[must_use]
    pub fn tokens_between(&self, from: Seq, to: Seq) -> u64 {
        self.events.range(from..=to).map(|(_, e)| e.tokens).sum()
    }
}

fn text_of(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(without_environment(text)),
            _ => None,
        })
        .filter(|t| !t.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// A segment opener's `<environment>` block is setup, not conversation: it is
/// left out. The compacted context after it (an earlier summary) is kept.
fn without_environment(text: &str) -> &str {
    match text.strip_prefix("<environment>") {
        Some(rest) => rest
            .split_once("</environment>")
            .map_or(text, |(_, after)| after.trim_start()),
        None => text,
    }
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::Image { media_type, .. } => Some(format!("[image {media_type}]")),
            ContentBlock::Document { media_type, .. } => Some(format!("[document {media_type}]")),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Prefixes the first line with `#seq label:` and the rest with `#seq |`.
fn number(seq: Seq, label: &str, raw: &str) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, line) in raw.lines().enumerate() {
        if i == 0 {
            lines.push(format!("#{} {label}: {line}", seq.0));
        } else {
            lines.push(format!("#{} | {line}", seq.0));
        }
    }
    if lines.is_empty() {
        lines.push(format!("#{} {label}:", seq.0));
    }
    lines
}

/// Renders `events` (one session's events in order, typically one segment).
///
/// # Examples
/// ```
/// use harness_context::compaction::render;
/// use harness_protocol::Event;
/// use harness_types::{Message, Seq, TaskId};
/// let task = TaskId::new();
/// let events = vec![(Seq(4), Event::UserInput { task, message: Message::user_text("fix the test\nplease") })];
/// let r = render(&events);
/// assert_eq!(r.text(), "#4 user: fix the test\n#4 | please");
/// ```
#[must_use]
pub fn render(events: &[(Seq, Event)]) -> Rendered {
    let mut out = Rendered {
        span: events.first().zip(events.last()).map(|(a, b)| (a.0, b.0)),
        ..Rendered::default()
    };
    let mut tool_names: HashMap<&str, &str> = HashMap::new();
    let mut after_results = false;
    for (seq, event) in events {
        let (raw, label, step_start) = match event {
            Event::UserInput { message, .. } => (text_of(message), "user".to_owned(), true),
            Event::AssistantMessage { message, .. } => {
                let mut parts = Vec::new();
                let text = text_of(message);
                if !text.is_empty() {
                    parts.push(text);
                }
                for block in &message.content {
                    if let ContentBlock::ToolCall { id, name, input } = block {
                        tool_names.insert(id.as_str(), name.as_str());
                        parts.push(format!("[call {name} {input}]"));
                    }
                }
                let start = after_results;
                after_results = false;
                (parts.join("\n"), "assistant".to_owned(), start)
            }
            Event::ToolFinished {
                call_id, ok, sent, ..
            } => {
                after_results = true;
                let name = tool_names.get(call_id.as_str()).copied().unwrap_or("tool");
                let status = if *ok { "" } else { " (failed)" };
                (blocks_text(sent), format!("result {name}{status}"), false)
            }
            Event::SystemNote { text, .. } => (text.clone(), "note".to_owned(), false),
            Event::ModeChanged { note, .. } => (note.clone(), "note".to_owned(), false),
            _ => continue,
        };
        let lines = number(*seq, &label, &raw);
        let tokens = lines.iter().map(|l| estimate_tokens(l) + 1).sum();
        out.events.insert(
            *seq,
            EventText {
                raw,
                lines,
                tokens,
                step_start,
            },
        );
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_protocol::BlobOrText;
    use harness_types::{Role, TaskId};
    use serde_json::json;

    #[test]
    fn the_environment_block_is_left_out_but_an_earlier_summary_stays() {
        let task = TaskId::new();
        let opener = Message::new(
            Role::User,
            vec![
                ContentBlock::text(
                    "<environment>\nWorking directory: /p\n</environment>\n<compacted-context>\n## Goal\nship it\n</compacted-context>",
                ),
                ContentBlock::text("<environment>only env</environment>"),
                ContentBlock::text("next step please"),
            ],
        );
        let r = render(&[(
            Seq(9),
            Event::UserInput {
                task,
                message: opener,
            },
        )]);
        let text = r.text();
        assert!(!text.contains("Working directory"), "{text}");
        assert!(text.starts_with("#9 user: <compacted-context>"), "{text}");
        assert!(text.ends_with("#9 | next step please"), "{text}");
    }

    #[test]
    fn calls_results_and_notes_are_numbered_and_steps_marked() {
        let task = TaskId::new();
        let events = vec![
            (
                Seq(1),
                Event::UserInput {
                    task,
                    message: Message::user_text("run it"),
                },
            ),
            (
                Seq(2),
                Event::AssistantMessage {
                    task,
                    message: Message::new(
                        Role::Assistant,
                        vec![
                            ContentBlock::text("Running."),
                            ContentBlock::ToolCall {
                                id: "c1".into(),
                                name: "shell".into(),
                                input: json!({"command": "ls"}),
                            },
                        ],
                    ),
                },
            ),
            (Seq(3), Event::TurnStarted { task, turn: 1 }),
            (
                Seq(4),
                Event::ToolFinished {
                    call_id: "c1".into(),
                    ok: false,
                    full: BlobOrText::Text {
                        text: "huge".into(),
                    },
                    sent: vec![ContentBlock::text("a.rs\nb.rs")],
                },
            ),
            (
                Seq(5),
                Event::AssistantMessage {
                    task,
                    message: Message::new(Role::Assistant, vec![ContentBlock::text("Done.")]),
                },
            ),
            (
                Seq(6),
                Event::SystemNote {
                    task,
                    text: "reverted a.rs".into(),
                },
            ),
        ];
        let r = render(&events);
        assert_eq!(
            r.text(),
            "#1 user: run it\n#2 assistant: Running.\n#2 | [call shell {\"command\":\"ls\"}]\n#4 result shell (failed): a.rs\n#4 | b.rs\n#5 assistant: Done.\n#6 note: reverted a.rs"
        );
        assert_eq!(r.span, Some((Seq(1), Seq(6))));
        assert_eq!(
            r.events[&Seq(4)].raw,
            "a.rs\nb.rs",
            "the capped form, not the full output"
        );
        let starts: Vec<u64> = r
            .events
            .iter()
            .filter(|(_, e)| e.step_start)
            .map(|(s, _)| s.0)
            .collect();
        assert_eq!(starts, [1, 5]);
        assert!(r.tokens_between(Seq(1), Seq(2)) > 0);
    }
}
