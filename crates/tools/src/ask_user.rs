//! The `ask_user` tool: ask the user a question and wait for the answer
//! (Tech spec 3.5).

use async_trait::async_trait;
use harness_ports::{
    AskError, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, UserQuestion,
};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

/// What the model is told when nobody can answer.
pub const NO_USER_ANSWER: &str = "No user is available to answer (this is a headless run). \
    Continue with your best judgement, and state the assumption you made in your final reply.";

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    question: String,
    #[serde(default)]
    options: Vec<String>,
    #[serde(default)]
    multi: bool,
}

/// Asks the user a question. The task waits (like an approval) until the user
/// answers; in headless sessions it returns [`NO_USER_ANSWER`] at once.
#[derive(Debug, Clone, Copy, Default)]
pub struct AskUser;

#[async_trait]
impl Tool for AskUser {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("ask_user").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Ask the user a question when you need a decision or information \
                only they have. Offer short options when the likely answers are known; the \
                user may still answer in their own words. Do not use this for permission to \
                run tools: approvals are handled separately."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "question": {"type": "string"},
                    "options": {"type": "array", "items": {"type": "string"}},
                    "multi": {"type": "boolean", "description": "Allow choosing several options."}
                },
                "required": ["question"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: Vec::new() }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        if input.question.trim().is_empty() {
            return Err(ToolError::InvalidInput("the question is empty".into()));
        }
        let question = UserQuestion {
            question: input.question.trim().to_owned(),
            options: input
                .options
                .into_iter()
                .map(|o| o.trim().to_owned())
                .filter(|o| !o.is_empty())
                .collect(),
            multi: input.multi,
        };
        match ctx.user.ask(question, &ctx.cancel).await {
            Ok(answer) => Ok(ToolOutput {
                content: vec![ContentBlock::text(format!("The user answered: {answer}"))],
                summary: "user answered".into(),
                touched: Vec::new(),
            }),
            Err(AskError::NoUser) => Ok(ToolOutput {
                content: vec![ContentBlock::text(NO_USER_ANSWER)],
                summary: "no user available".into(),
                touched: Vec::new(),
            }),
            Err(AskError::Cancelled) => Err(ToolError::Cancelled),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use harness_ports::UserChannel;
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    struct Scripted {
        answer: Result<String, AskError>,
        seen: Mutex<Option<UserQuestion>>,
    }

    #[async_trait]
    impl UserChannel for Scripted {
        async fn ask(&self, q: UserQuestion, _: &CancellationToken) -> Result<String, AskError> {
            *self.seen.lock().unwrap() = Some(q);
            self.answer.clone()
        }
    }

    async fn ask(
        answer: Result<String, AskError>,
        input: Value,
    ) -> (Result<ToolOutput, ToolError>, Option<UserQuestion>) {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, _) = ctx(dir.path());
        let user = Arc::new(Scripted {
            answer,
            seen: Mutex::new(None),
        });
        c.user = user.clone();
        let out = AskUser.run(input, c).await;
        let seen = user.seen.lock().unwrap().clone();
        (out, seen)
    }

    #[tokio::test]
    async fn returns_the_users_answer_and_passes_the_options() {
        let (out, seen) = ask(
            Ok("Postgres".into()),
            json!({"question": " Which DB? ", "options": ["SQLite", " Postgres ", ""], "multi": false}),
        )
        .await;
        let out = out.unwrap();
        assert_eq!(
            out.content,
            vec![ContentBlock::text("The user answered: Postgres")]
        );
        assert_eq!(
            seen.unwrap(),
            UserQuestion {
                question: "Which DB?".into(),
                options: vec!["SQLite".into(), "Postgres".into()],
                multi: false
            }
        );
    }

    #[tokio::test]
    async fn headless_sessions_get_a_no_user_answer() {
        let (out, _) = ask(Err(AskError::NoUser), json!({"question": "?"})).await;
        assert_eq!(
            out.unwrap().content,
            vec![ContentBlock::text(NO_USER_ANSWER)]
        );
    }

    #[tokio::test]
    async fn an_interrupt_cancels_the_call() {
        let (out, _) = ask(Err(AskError::Cancelled), json!({"question": "?"})).await;
        assert_eq!(out.unwrap_err(), ToolError::Cancelled);
    }

    #[tokio::test]
    async fn an_empty_question_is_rejected() {
        let (out, seen) = ask(Ok("x".into()), json!({"question": "  "})).await;
        assert!(matches!(out, Err(ToolError::InvalidInput(_))));
        assert!(seen.is_none());
    }
}
