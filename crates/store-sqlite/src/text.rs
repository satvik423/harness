//! The searchable text of an event, for the `events_fts` index.

use harness_protocol::{BlobOrText, Event};
use harness_types::{ContentBlock, Message};

/// Text worth finding later: what was said, run and returned. Empty for
/// bookkeeping events (state changes, usage).
pub(crate) fn searchable(event: &Event) -> String {
    match event {
        Event::UserInput { message, .. } | Event::AssistantMessage { message, .. } => {
            message_text(message)
        }
        Event::ToolFinished { full, sent, .. } => match full {
            BlobOrText::Text { text } => text.clone(),
            // Large outputs live in a blob; index what the model saw.
            BlobOrText::Blob { .. } => blocks_text(sent),
        },
        Event::SystemNote { text, .. } => text.clone(),
        Event::AssistantAborted { partial_text, .. } => partial_text.clone(),
        Event::Warning { message, .. } | Event::Error { message, .. } => message.clone(),
        Event::QuestionAsked { question, .. } => question.clone(),
        Event::QuestionAnswered { answer, .. } => answer.clone(),
        _ => String::new(),
    }
}

fn message_text(message: &Message) -> String {
    blocks_text(&message.content)
}

fn blocks_text(blocks: &[ContentBlock]) -> String {
    blocks
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            ContentBlock::ToolCall { name, input, .. } => Some(format!("{name} {input}")),
            ContentBlock::ToolResult { content, .. } => Some(blocks_text(content)),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// The first prompt's text, as a one-line session title: the last text block
/// of the first user message (the first block is the environment opener).
pub(crate) fn title_of(message: &Message) -> Option<String> {
    let text = message.content.iter().rev().find_map(|b| match b {
        ContentBlock::Text { text } if !text.trim().is_empty() => Some(text.as_str()),
        _ => None,
    })?;
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    Some(if one_line.chars().count() > 80 {
        format!("{}…", one_line.chars().take(79).collect::<String>())
    } else {
        one_line
    })
}

/// Turns free text into an FTS5 query that cannot be a syntax error: every
/// word becomes a quoted phrase, and all must match.
pub(crate) fn fts_query(query: &str) -> Option<String> {
    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| format!("\"{}\"", t.replace('"', "\"\"")))
        .collect();
    (!terms.is_empty()).then(|| terms.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;
    use harness_types::{Role, TaskId};

    #[test]
    fn messages_contribute_text_and_tool_calls() {
        let m = Message::new(
            Role::Assistant,
            vec![
                ContentBlock::text("Running the tests"),
                ContentBlock::ToolCall {
                    id: "c".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"command": "cargo test"}),
                },
            ],
        );
        let t = searchable(&Event::AssistantMessage {
            task: TaskId::new(),
            message: m,
        });
        assert!(
            t.contains("Running the tests") && t.contains("cargo test"),
            "{t}"
        );
    }

    #[test]
    fn bookkeeping_events_have_no_text() {
        assert!(
            searchable(&Event::ToolStarted {
                call_id: "c".into()
            })
            .is_empty()
        );
    }

    #[test]
    fn title_is_the_prompt_on_one_line_and_short() {
        let m = Message::new(
            Role::User,
            vec![
                ContentBlock::text("<environment>…</environment>"),
                ContentBlock::text("fix   the\nlogin test"),
            ],
        );
        assert_eq!(title_of(&m).unwrap(), "fix the login test");
        let long = Message::new(Role::User, vec![ContentBlock::text("x".repeat(200))]);
        assert_eq!(title_of(&long).unwrap().chars().count(), 80);
    }

    #[test]
    fn queries_are_quoted_so_syntax_cannot_break_them() {
        assert_eq!(
            fts_query("exit code \"1\"").unwrap(),
            r#""exit" "code" """1""""#
        );
        assert_eq!(fts_query("AND OR (").unwrap(), r#""AND" "OR" "(""#);
        assert!(fts_query("  ").is_none());
    }
}
