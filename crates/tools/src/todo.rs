//! The `todo` tool: the model's visible task checklist (Tech spec 3.5).

use async_trait::async_trait;
use harness_ports::{RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

/// State of one checklist item.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Status {
    Pending,
    InProgress,
    Completed,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Item {
    text: String,
    status: Status,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    items: Vec<Item>,
}

/// Replaces the task checklist. The model sends the full list every time; the
/// UI renders it from the tool call.
#[derive(Debug, Clone, Copy, Default)]
pub struct Todo;

/// Renders the checklist: `[x]` done, `[~]` in progress, `[ ]` pending.
fn render(items: &[Item]) -> String {
    let done = items
        .iter()
        .filter(|i| i.status == Status::Completed)
        .count();
    let mut out = format!("Todo list updated ({done}/{} done):", items.len());
    for item in items {
        let mark = match item.status {
            Status::Completed => "[x]",
            Status::InProgress => "[~]",
            Status::Pending => "[ ]",
        };
        out.push_str(&format!("\n{mark} {}", item.text.trim()));
    }
    out
}

#[async_trait]
impl Tool for Todo {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("todo").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Keep a checklist for multi-step work. Send the complete list every \
                time; it replaces the previous one. Mark one item in_progress while you work \
                on it, and completed as soon as it is done."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": {"type": "string"},
                                "status": {"type": "string", "enum": ["pending", "in_progress", "completed"]}
                            },
                            "required": ["text", "status"],
                            "additionalProperties": false
                        }
                    }
                },
                "required": ["items"],
                "additionalProperties": false
            }),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: Vec::new() }
    }

    async fn run(&self, input: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        if input.items.iter().any(|i| i.text.trim().is_empty()) {
            return Err(ToolError::InvalidInput("every item needs text".into()));
        }
        let active = input
            .items
            .iter()
            .filter(|i| i.status == Status::InProgress)
            .count();
        if active > 1 {
            return Err(ToolError::InvalidInput(format!(
                "{active} items are in_progress; keep at most one in progress"
            )));
        }
        let done = input
            .items
            .iter()
            .filter(|i| i.status == Status::Completed)
            .count();
        Ok(ToolOutput {
            content: vec![ContentBlock::text(render(&input.items))],
            summary: format!("todo {done}/{}", input.items.len()),
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    async fn todo(input: Value) -> Result<ToolOutput, ToolError> {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        Todo.run(input, c).await
    }

    #[tokio::test]
    async fn renders_the_checklist_with_progress() {
        let out = todo(json!({"items": [
            {"text": "read the code", "status": "completed"},
            {"text": "fix the bug", "status": "in_progress"},
            {"text": "run tests", "status": "pending"}
        ]}))
        .await
        .unwrap();
        let ContentBlock::Text { text } = &out.content[0] else {
            panic!()
        };
        assert_eq!(
            text,
            "Todo list updated (1/3 done):\n[x] read the code\n[~] fix the bug\n[ ] run tests"
        );
        assert_eq!(out.summary, "todo 1/3");
    }

    #[tokio::test]
    async fn rejects_two_items_in_progress_and_empty_text() {
        let two = json!({"items": [
            {"text": "a", "status": "in_progress"},
            {"text": "b", "status": "in_progress"}
        ]});
        assert!(matches!(todo(two).await, Err(ToolError::InvalidInput(_))));
        let empty = json!({"items": [{"text": " ", "status": "pending"}]});
        assert!(matches!(todo(empty).await, Err(ToolError::InvalidInput(_))));
        let bad = json!({"items": [{"text": "a", "status": "done"}]});
        assert!(matches!(todo(bad).await, Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn is_read_only_and_strict() {
        let ctx = RiskCtx {
            cwd: "/".into(),
            writable_roots: vec![],
        };
        assert!(Todo.risk(&json!({}), &ctx).is_read_only());
        assert!(Todo.spec().strict);
    }
}
