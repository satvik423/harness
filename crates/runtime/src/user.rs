//! [`SessionUser`]: the runtime's [`UserChannel`], used by `ask_user`.
//!
//! A question moves the task to `AwaitingApproval` (the spec calls `ask_user`
//! "approval-like"), emits `QuestionAsked`, and waits for `Op::Answer`.

use std::sync::{Arc, Mutex, PoisonError};

use async_trait::async_trait;
use harness_ports::{AskError, EventSink, UserChannel, UserQuestion};
use harness_protocol::{Event, TaskState};
use harness_types::{QuestionId, TaskId};
use tokio_util::sync::CancellationToken;

use crate::ApprovalMode;
use crate::approval::Questions;

/// Moves a task to `to`, emitting the transition from its current state.
pub(crate) fn transition(
    sink: &dyn EventSink,
    task: TaskId,
    state: &Mutex<TaskState>,
    to: TaskState,
    reason: impl Into<String>,
) {
    let mut current = state.lock().unwrap_or_else(PoisonError::into_inner);
    let from = *current;
    *current = to;
    drop(current);
    sink.emit(Event::TaskState {
        task,
        from,
        to,
        reason: reason.into(),
    });
}

/// Asks the session's user on behalf of one tool call.
pub(crate) struct SessionUser {
    pub(crate) task: TaskId,
    pub(crate) call_id: String,
    pub(crate) mode: ApprovalMode,
    pub(crate) sink: Arc<dyn EventSink>,
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
        transition(
            self.sink.as_ref(),
            self.task,
            &self.state,
            TaskState::AwaitingApproval,
            "waiting for the user's answer",
        );
        self.sink.emit(Event::QuestionAsked {
            id,
            task: self.task,
            call_id: self.call_id.clone(),
            question: question.question,
            options: question.options,
            multi: question.multi,
        });
        let answer = tokio::select! {
            answer = rx => answer.ok(),
            () = cancel.cancelled() => None,
        };
        self.questions.forget(&id);
        let answer = answer.ok_or(AskError::Cancelled)?;
        self.sink.emit(Event::QuestionAnswered {
            id,
            answer: answer.clone(),
        });
        transition(
            self.sink.as_ref(),
            self.task,
            &self.state,
            TaskState::Running,
            "question answered",
        );
        Ok(answer)
    }
}
