//! [`SessionUser`]: the runtime's [`UserChannel`], used by `ask_user`.
//!
//! A question moves the task to `AwaitingApproval` (the spec calls `ask_user`
//! "approval-like"), records `QuestionAsked`, and waits for `Op::Answer`.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use harness_ports::{AskError, StoreError, UserChannel, UserQuestion};
use harness_protocol::{Event, TaskState};
use harness_types::{QuestionId, TaskId};
use tokio_util::sync::CancellationToken;

use crate::ApprovalMode;
use crate::approval::Questions;
use crate::journal::Journal;

/// Moves a task to `to` and records the transition from its current state.
///
/// # Errors
/// The event log's error; the in-memory state has changed anyway.
pub(crate) async fn transition(
    journal: &Journal,
    task: TaskId,
    state: &Mutex<TaskState>,
    to: TaskState,
    reason: impl Into<String>,
) -> Result<(), StoreError> {
    let from = {
        let mut current = state.lock().unwrap_or_else(PoisonError::into_inner);
        std::mem::replace(&mut *current, to)
    };
    journal
        .record(
            Some(task),
            Event::TaskState {
                task,
                from,
                to,
                reason: reason.into(),
            },
        )
        .await
        .map(|_| ())
}

/// Asks the session's user on behalf of one tool call.
pub(crate) struct SessionUser {
    pub(crate) task: TaskId,
    pub(crate) call_id: String,
    pub(crate) mode: ApprovalMode,
    pub(crate) journal: Arc<Journal>,
    pub(crate) questions: Arc<Questions>,
    pub(crate) state: Arc<Mutex<TaskState>>,
}

#[async_trait]
impl UserChannel for SessionUser {
    async fn ask(
        &self,
        question: UserQuestion,
        cancel: &CancellationToken,
    ) -> Result<String, AskError> {
        // `never` means nobody is watching (headless runs).
        if self.mode == ApprovalMode::Never {
            return Err(AskError::NoUser);
        }
        let id = QuestionId::new();
        let rx = self.questions.register(id);
        let asked = async {
            transition(
                &self.journal,
                self.task,
                &self.state,
                TaskState::AwaitingApproval,
                "waiting for the user's answer",
            )
            .await?;
            self.journal
                .record(
                    Some(self.task),
                    Event::QuestionAsked {
                        id,
                        task: self.task,
                        call_id: self.call_id.clone(),
                        question: question.question,
                        options: question.options,
                        multi: question.multi,
                    },
                )
                .await
        };
        if asked.await.is_err() {
            // The question could not be saved, so it is not shown either.
            self.questions.forget(&id);
            return Err(AskError::NoUser);
        }
        let answer = tokio::select! {
            answer = rx => answer.ok(),
            () = cancel.cancelled() => None,
        };
        self.questions.forget(&id);
        let answer = answer.ok_or(AskError::Cancelled)?;
        let _ = self
            .journal
            .record(
                Some(self.task),
                Event::QuestionAnswered {
                    id,
                    answer: answer.clone(),
                },
            )
            .await;
        let _ = transition(
            &self.journal,
            self.task,
            &self.state,
            TaskState::Running,
            "question answered",
        )
        .await;
        Ok(answer)
    }
}
