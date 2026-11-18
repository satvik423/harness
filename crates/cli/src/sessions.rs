//! Text for `harness session …` and `harness task …`: tables, event lines,
//! exports, and resolving ids from a unique prefix.

use std::fmt::Write as _;

use harness_context::civil_date;
use harness_ports::{LoggedEvent, SessionRecord, TaskRecord};
use harness_protocol::{BlobOrText, Event, TaskState};
use harness_types::{ContentBlock, Message};
use thiserror::Error;

use crate::render::summarize_input;

/// Why an id typed by the user did not pick exactly one item.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// Nothing starts with it.
    #[error("no {what} starts with {prefix:?}")]
    NotFound {
        /// "session" or "task".
        what: &'static str,
        /// What was typed.
        prefix: String,
    },
    /// Several items start with it.
    #[error("{prefix:?} matches {count} {what}s; type more characters")]
    Ambiguous {
        /// "session" or "task".
        what: &'static str,
        /// What was typed.
        prefix: String,
        /// How many matched.
        count: usize,
    },
}

fn resolve<'a, T>(
    items: &'a [T],
    prefix: &str,
    id: impl Fn(&T) -> String,
    what: &'static str,
) -> Result<&'a T, ResolveError> {
    let prefix_upper = prefix.trim().to_ascii_uppercase();
    let matches: Vec<&T> = items
        .iter()
        .filter(|item| !prefix_upper.is_empty() && id(item).starts_with(&prefix_upper))
        .collect();
    match matches.as_slice() {
        [one] => Ok(one),
        [] => Err(ResolveError::NotFound {
            what,
            prefix: prefix.to_owned(),
        }),
        many => Err(ResolveError::Ambiguous {
            what,
            prefix: prefix.to_owned(),
            count: many.len(),
        }),
    }
}

/// Finds the session whose id starts with `prefix` (case-insensitive).
///
/// # Errors
/// [`ResolveError`] if none or several match.
///
/// # Examples
/// ```
/// # use harness_cli::sessions::resolve_session;
/// assert!(resolve_session(&[], "01J").is_err());
/// ```
pub fn resolve_session<'a>(
    sessions: &'a [SessionRecord],
    prefix: &str,
) -> Result<&'a SessionRecord, ResolveError> {
    resolve(sessions, prefix, |s| s.id.to_string(), "session")
}

/// Finds the task whose id starts with `prefix` (case-insensitive).
///
/// # Errors
/// [`ResolveError`] if none or several match.
pub fn resolve_task<'a>(
    tasks: &'a [TaskRecord],
    prefix: &str,
) -> Result<&'a TaskRecord, ResolveError> {
    resolve(tasks, prefix, |t| t.id.to_string(), "task")
}

/// `YYYY-MM-DD HH:MM` (UTC) from Unix milliseconds.
///
/// # Examples
/// ```
/// assert_eq!(harness_cli::sessions::format_time(90_000_000), "1970-01-02 01:00");
/// ```
#[must_use]
pub fn format_time(ms: i64) -> String {
    let minutes = ms.div_euclid(60_000).rem_euclid(1_440);
    format!("{} {:02}:{:02}", civil_date(ms), minutes / 60, minutes % 60)
}

fn state_name(state: TaskState) -> String {
    serde_json::to_value(state)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// The `harness session list` table: newest first, with each session's latest task state.
#[must_use]
pub fn session_table(rows: &[(SessionRecord, Option<TaskRecord>)]) -> String {
    if rows.is_empty() {
        return "No sessions in this project yet. Start one with `harness run PROMPT`.\n".into();
    }
    let mut out = format!(
        "{:<26}  {:<16}  {:<17}  {}\n",
        "SESSION", "CREATED (UTC)", "LAST TASK", "TITLE"
    );
    for (session, task) in rows {
        let state = task
            .as_ref()
            .map_or_else(|| "-".to_owned(), |t| state_name(t.state));
        let _ = writeln!(
            out,
            "{:<26}  {:<16}  {:<17}  {}",
            session.id,
            format_time(session.created_at),
            state,
            session.title.as_deref().unwrap_or("(no prompt yet)")
        );
    }
    out
}

/// The `harness task list` table.
#[must_use]
pub fn task_table(tasks: &[TaskRecord]) -> String {
    if tasks.is_empty() {
        return "No tasks.\n".into();
    }
    let mut out = format!(
        "{:<26}  {:<26}  {:<17}  {:<16}  {}\n",
        "TASK", "SESSION", "STATE", "UPDATED (UTC)", "REASON"
    );
    for t in tasks {
        let _ = writeln!(
            out,
            "{:<26}  {:<26}  {:<17}  {:<16}  {}",
            t.id,
            t.session,
            state_name(t.state),
            format_time(t.updated_at),
            t.reason.as_deref().unwrap_or("")
        );
    }
    out
}

fn shorten(text: &str, max: usize) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if one_line.chars().count() <= max {
        one_line
    } else {
        format!("{}…", one_line.chars().take(max).collect::<String>())
    }
}

/// The user's own words in a stored user message (the opener and system
/// notes are skipped).
fn prompt_text(message: &Message) -> String {
    message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text }
                if !text.starts_with("<environment>") && !text.starts_with("<system-note>") =>
            {
                Some(text.as_str())
            }
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// One line per event for `harness session show`.
#[must_use]
pub fn event_line(e: &LoggedEvent) -> String {
    let detail = match &e.envelope.event {
        Event::UserInput { message, .. } => shorten(&prompt_text(message), 90),
        Event::AssistantMessage { message, .. } => {
            let calls: Vec<String> = message
                .content
                .iter()
                .filter_map(|b| match b {
                    ContentBlock::ToolCall { name, .. } => Some(name.clone()),
                    _ => None,
                })
                .collect();
            let text = shorten(&message.text(), 70);
            if calls.is_empty() {
                text
            } else {
                format!("{text} [calls: {}]", calls.join(", "))
            }
        }
        Event::ToolCall { name, input, .. } => format!("{name}: {}", summarize_input(name, input)),
        Event::ToolFinished {
            call_id, ok, sent, ..
        } => format!(
            "{call_id} {} {}",
            if *ok { "ok" } else { "failed" },
            shorten(&blocks_text(sent), 60)
        ),
        Event::TaskState {
            task, to, reason, ..
        } => {
            format!("task {task} → {} ({reason})", state_name(*to))
        }
        Event::ApprovalRequested { reason, .. } => shorten(reason, 80),
        Event::ApprovalResolved { decision, .. } => format!("{decision:?}"),
        Event::Warning { message, .. } | Event::Error { message, .. } => shorten(message, 80),
        Event::UndoApplied { paths, .. } => paths.join(", "),
        Event::SystemNote { text, .. } => shorten(text, 80),
        _ => String::new(),
    };
    format!(
        "#{:<5} {:<19} {detail}",
        e.envelope.seq.0,
        e.envelope.event.kind()
    )
}

/// Every event as one JSON object per line: `{seq, ts, task, segment, event}`.
#[must_use]
pub fn export_jsonl(events: &[LoggedEvent]) -> String {
    let mut out = String::new();
    for e in events {
        let line = serde_json::json!({
            "seq": e.envelope.seq,
            "ts": e.envelope.ts,
            "session": e.envelope.session,
            "task": e.task,
            "segment": e.segment,
            "event": e.envelope.event,
        });
        let _ = writeln!(out, "{line}");
    }
    out
}

/// A readable Markdown transcript: prompts, replies, tool calls and results.
#[must_use]
pub fn export_markdown(session: &SessionRecord, events: &[LoggedEvent]) -> String {
    let mut out = format!(
        "# {}\n\nSession `{}` · persona `{}` · started {} UTC · `{}`\n",
        session.title.as_deref().unwrap_or("harness session"),
        session.id,
        session.persona,
        format_time(session.created_at),
        session.cwd.display()
    );
    for e in events {
        match &e.envelope.event {
            Event::UserInput { message, .. } => {
                let _ = write!(out, "\n## You\n\n{}\n", prompt_text(message));
            }
            Event::AssistantMessage { message, .. } => {
                let _ = write!(out, "\n## Assistant\n\n");
                for block in &message.content {
                    match block {
                        ContentBlock::Text { text } => {
                            let _ = writeln!(out, "{text}\n");
                        }
                        ContentBlock::ToolCall { name, input, .. } => {
                            let _ = writeln!(
                                out,
                                "- **{name}** `{}`",
                                summarize_input(name, input).replace('`', "'")
                            );
                        }
                        _ => {}
                    }
                }
            }
            Event::ToolFinished {
                call_id, ok, full, ..
            } => {
                let text = match full {
                    BlobOrText::Text { text } => text.clone(),
                    BlobOrText::Blob { blob } => format!("(full output in blob {blob})"),
                };
                let _ = write!(
                    out,
                    "\n<details><summary>result of {call_id}{}</summary>\n\n```text\n{}\n```\n\n</details>\n",
                    if *ok { "" } else { " (failed)" },
                    text.replace("```", "'''")
                );
            }
            Event::UndoApplied { paths, .. } => {
                let _ = write!(out, "\n> Undo restored: {}\n", paths.join(", "));
            }
            Event::TaskState { to, reason, .. }
                if matches!(
                    to,
                    TaskState::Completed
                        | TaskState::Failed
                        | TaskState::Cancelled
                        | TaskState::Paused
                        | TaskState::BudgetExhausted
                ) =>
            {
                let _ = write!(out, "\n_Task {}: {reason}_\n", state_name(*to));
            }
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_ports::SessionStatus;
    use harness_protocol::Envelope;
    use harness_types::{Role, Seq, SessionId, TaskId};

    fn session(id: SessionId, title: Option<&str>) -> SessionRecord {
        SessionRecord {
            id,
            created_at: 1_763_380_800_000,
            title: title.map(str::to_owned),
            persona: "default".into(),
            cwd: "/proj".into(),
            status: SessionStatus::Open,
            parent: None,
            forked_at: None,
            current_segment: 0,
        }
    }

    fn logged(seq: u64, event: Event) -> LoggedEvent {
        LoggedEvent {
            envelope: Envelope {
                seq: Seq(seq),
                ts: 0,
                session: SessionId::new(),
                event,
            },
            task: None,
            segment: 0,
        }
    }

    #[test]
    fn prefixes_resolve_to_one_item_or_explain() {
        let a = session(SessionId::new(), None);
        let b = session(SessionId::new(), None);
        let all = [a.clone(), b.clone()];
        let full = a.id.to_string();
        assert_eq!(resolve_session(&all, &full).unwrap().id, a.id);
        assert_eq!(
            resolve_session(&all, &full.to_lowercase()).unwrap().id,
            a.id
        );
        // ULIDs made in the same millisecond share a long prefix.
        assert!(matches!(
            resolve_session(&all, &full[..4]),
            Err(ResolveError::Ambiguous { count: 2, .. })
        ));
        assert!(matches!(
            resolve_session(&all, "ZZZ"),
            Err(ResolveError::NotFound { .. })
        ));
        assert!(resolve_session(&all, "").is_err());
    }

    #[test]
    fn times_are_utc_minutes() {
        assert_eq!(format_time(0), "1970-01-01 00:00");
        assert_eq!(format_time(1_763_380_800_000), "2025-11-17 12:00");
    }

    #[test]
    fn tables_list_sessions_and_tasks() {
        let s = session(SessionId::new(), Some("fix the login test"));
        let task = TaskRecord {
            id: TaskId::new(),
            session: s.id,
            persona: "default".into(),
            state: TaskState::Paused,
            reason: Some("recovered".into()),
            created_at: 0,
            updated_at: 0,
        };
        let table = session_table(&[(s.clone(), Some(task.clone()))]);
        assert!(
            table.contains("fix the login test") && table.contains("paused"),
            "{table}"
        );
        assert!(session_table(&[]).contains("No sessions"));
        let tasks = task_table(&[task]);
        assert!(
            tasks.contains("recovered") && tasks.contains(&s.id.to_string()),
            "{tasks}"
        );
    }

    #[test]
    fn exports_contain_the_conversation() {
        let s = session(SessionId::new(), Some("demo"));
        let task = TaskId::new();
        let events = vec![
            logged(
                1,
                Event::UserInput {
                    task,
                    message: Message::new(
                        Role::User,
                        vec![
                            ContentBlock::text("<environment>e</environment>"),
                            ContentBlock::text("list the files"),
                        ],
                    ),
                },
            ),
            logged(
                2,
                Event::AssistantMessage {
                    task,
                    message: Message::new(
                        Role::Assistant,
                        vec![
                            ContentBlock::text("Looking."),
                            ContentBlock::ToolCall {
                                id: "c1".into(),
                                name: "shell".into(),
                                input: serde_json::json!({"command": "ls"}),
                            },
                        ],
                    ),
                },
            ),
            logged(
                3,
                Event::ToolFinished {
                    call_id: "c1".into(),
                    ok: true,
                    full: BlobOrText::Text {
                        text: "a.rs\nb.rs".into(),
                    },
                    sent: vec![],
                },
            ),
        ];
        let md = export_markdown(&s, &events);
        assert!(md.starts_with("# demo\n"), "{md}");
        assert!(md.contains("## You\n\nlist the files\n"), "{md}");
        assert!(!md.contains("<environment>"), "{md}");
        assert!(md.contains("- **shell** `ls`"), "{md}");
        assert!(md.contains("a.rs\nb.rs"), "{md}");
        let jsonl = export_jsonl(&events);
        assert_eq!(jsonl.lines().count(), 3);
        let first: serde_json::Value = serde_json::from_str(jsonl.lines().next().unwrap()).unwrap();
        assert_eq!(first["seq"], 1);
        assert_eq!(first["event"]["type"], "user_input");
        assert!(event_line(&events[1]).contains("[calls: shell]"));
    }
}
