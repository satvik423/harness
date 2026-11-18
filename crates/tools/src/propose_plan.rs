//! The `propose_plan` tool: in plan mode, show the user a plan and switch
//! modes when they approve it (ADR-042).

use async_trait::async_trait;
use harness_ports::{
    AskError, ModeError, NO_ANSWER, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput,
    ToolSpec, UserQuestion,
};
use harness_types::{ContentBlock, Mode, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

/// The choices offered with a plan, in order: approve and auto-accept edits,
/// approve and ask before each edit, keep planning. The user may also answer
/// in their own words (feedback on the plan).
pub const PLAN_CHOICES: [&str; 3] = [
    "Yes, and auto-accept edits",
    "Yes, but ask before each edit",
    "No, keep planning",
];

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    plan: String,
}

/// Asks the user to approve a plan. Approving leaves plan mode (for `auto` or
/// `read-only`); anything else keeps it, and the user's words go back to the
/// model. Outside plan mode it changes nothing.
#[derive(Debug, Clone, Copy, Default)]
pub struct ProposePlan;

fn text(content: String, summary: &str) -> ToolOutput {
    ToolOutput {
        content: vec![ContentBlock::text(content)],
        summary: summary.into(),
        touched: Vec::new(),
    }
}

#[async_trait]
impl Tool for ProposePlan {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("propose_plan").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "In plan mode, present your plan to the user for approval once you \
                have explored enough to know what to change. Write it in Markdown: the steps, \
                the files you will touch, and how you will check the result. The user approves \
                it (plan mode then ends and you carry it out) or asks you to keep planning. \
                Outside plan mode, do the work instead of calling this."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "string", "description": "The plan, in Markdown."}
                },
                "required": ["plan"],
                "additionalProperties": false
            }),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        // It changes nothing by itself; approving the plan is the user's act.
        Risk::ReadOnly { paths: Vec::new() }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let plan = input.plan.trim();
        if plan.is_empty() {
            return Err(ToolError::InvalidInput("the plan is empty".into()));
        }
        let current = ctx.mode.current();
        if current != Mode::Plan {
            return Ok(text(
                format!(
                    "Plan mode is off (the permission mode is {current}), so there is nothing to approve. Go ahead with the work."
                ),
                "not in plan mode",
            ));
        }
        let question = UserQuestion {
            question: plan.to_owned(),
            options: PLAN_CHOICES.iter().map(|c| (*c).to_owned()).collect(),
            multi: false,
        };
        let answer = match ctx.user.ask(question, &ctx.cancel).await {
            Ok(answer) if answer.trim() != NO_ANSWER => answer,
            // Nobody can approve: proposing again would only loop.
            Ok(_) | Err(AskError::NoUser) => {
                return Ok(text(
                    "No user is available to approve the plan (this is a headless run). Plan mode stays on: give the plan as your final reply and stop.".into(),
                    "no user available",
                ));
            }
            Err(AskError::Cancelled) => return Err(ToolError::Cancelled),
        };
        let approved = match answer.trim() {
            a if a == PLAN_CHOICES[0] => Some(Mode::Auto),
            a if a == PLAN_CHOICES[1] => Some(Mode::ReadOnly),
            _ => None,
        };
        let Some(mode) = approved else {
            let feedback = if answer.trim() == PLAN_CHOICES[2] {
                "The user wants you to keep planning. Plan mode stays on: ask what they want changed, or refine the plan and propose it again.".to_owned()
            } else {
                format!(
                    "The user has not approved the plan yet. Their feedback: {}\nPlan mode stays on. If they asked for changes, revise the plan and propose it again; otherwise answer them and end your turn.",
                    answer.trim()
                )
            };
            return Ok(text(feedback, "keep planning"));
        };
        ctx.mode.set(mode).await.map_err(|e| match e {
            ModeError::Store(m) => ToolError::Failed(m),
            other => ToolError::Failed(other.to_string()),
        })?;
        let rule = match mode {
            Mode::Auto => {
                "edits inside the project run without asking; commands still need approval"
            }
            _ => "every edit and command needs the user's approval",
        };
        Ok(text(
            format!(
                "The user approved the plan. Plan mode is off; the permission mode is now {mode} ({rule}). Carry out the plan."
            ),
            &format!("approved; mode {mode}"),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use harness_ports::{ModeControl, UserChannel};
    use std::sync::{Arc, Mutex};
    use tokio_util::sync::CancellationToken;

    struct Scripted(Result<String, AskError>, Mutex<Option<UserQuestion>>);

    #[async_trait]
    impl UserChannel for Scripted {
        async fn ask(&self, q: UserQuestion, _: &CancellationToken) -> Result<String, AskError> {
            *self.1.lock().unwrap() = Some(q);
            self.0.clone()
        }
    }

    struct Switchable(Mutex<Mode>);

    #[async_trait]
    impl ModeControl for Switchable {
        fn current(&self) -> Mode {
            *self.0.lock().unwrap()
        }
        async fn set(&self, mode: Mode) -> Result<(), ModeError> {
            *self.0.lock().unwrap() = mode;
            Ok(())
        }
    }

    struct Run {
        out: Result<ToolOutput, ToolError>,
        asked: Option<UserQuestion>,
        mode: Mode,
    }

    async fn propose(mode: Mode, answer: Result<String, AskError>) -> Run {
        let dir = tempfile::tempdir().unwrap();
        let (mut c, _, _) = ctx(dir.path());
        let user = Arc::new(Scripted(answer, Mutex::new(None)));
        let modes = Arc::new(Switchable(Mutex::new(mode)));
        c.user = user.clone();
        c.mode = modes.clone();
        let out = ProposePlan
            .run(json!({"plan": "1. Fix the check\n2. Test it"}), c)
            .await;
        let asked = user.1.lock().unwrap().clone();
        Run {
            out,
            asked,
            mode: modes.current(),
        }
    }

    fn said(run: &Run) -> String {
        match &run.out.as_ref().unwrap().content[0] {
            ContentBlock::Text { text } => text.clone(),
            other => panic!("{other:?}"),
        }
    }

    #[tokio::test]
    async fn approving_switches_to_the_chosen_mode() {
        let run = propose(Mode::Plan, Ok(PLAN_CHOICES[0].into())).await;
        let asked = run.asked.clone().unwrap();
        assert_eq!(asked.question, "1. Fix the check\n2. Test it");
        assert_eq!(asked.options, PLAN_CHOICES);
        assert_eq!(run.mode, Mode::Auto);
        assert!(said(&run).contains("now auto"), "{}", said(&run));

        let run = propose(Mode::Plan, Ok(PLAN_CHOICES[1].into())).await;
        assert_eq!(run.mode, Mode::ReadOnly);
    }

    #[tokio::test]
    async fn keep_planning_or_feedback_stays_in_plan_mode() {
        let run = propose(Mode::Plan, Ok(PLAN_CHOICES[2].into())).await;
        assert_eq!(run.mode, Mode::Plan);
        assert!(said(&run).contains("keep planning"));
        let run = propose(Mode::Plan, Ok("also update the docs".into())).await;
        assert_eq!(run.mode, Mode::Plan);
        assert!(said(&run).contains("Their feedback: also update the docs"));
    }

    #[tokio::test]
    async fn outside_plan_mode_or_headless_nothing_changes() {
        let run = propose(Mode::Auto, Ok(PLAN_CHOICES[0].into())).await;
        assert!(run.asked.is_none());
        assert_eq!(run.mode, Mode::Auto);
        assert!(said(&run).contains("Plan mode is off"));
        let run = propose(Mode::Plan, Err(AskError::NoUser)).await;
        assert_eq!(run.mode, Mode::Plan);
        assert!(said(&run).contains("headless"));
        // A closed input answers, but nobody approved anything.
        let run = propose(Mode::Plan, Ok(NO_ANSWER.into())).await;
        assert_eq!(run.mode, Mode::Plan);
        assert!(said(&run).contains("stop"), "{}", said(&run));
        let run = propose(Mode::Plan, Err(AskError::Cancelled)).await;
        assert_eq!(run.out, Err(ToolError::Cancelled));
    }
}
