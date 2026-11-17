//! [`Event`]: everything the core reports, and the [`Envelope`] that orders it.

use harness_types::{
    ApprovalId, BlobRef, ContentBlock, Message, QuestionId, Risk, Seq, SessionId, StopReason,
    TaskId, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::op::Decision;

/// An [`Event`] with its position in the session's log.
///
/// The same envelopes are broadcast to front-ends and (when
/// [`Event::is_persisted`]) stored in the event log, so the log is a complete
/// replay of what the UI saw.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    /// Per-session, gapless sequence number assigned by the event log.
    pub seq: Seq,
    /// Unix time in milliseconds when the event was recorded.
    pub ts: i64,
    /// The session the event belongs to.
    pub session: SessionId,
    /// The event itself.
    pub event: Event,
}

/// Lifecycle state of a task (see SDD 2.8 and Tech spec 3.14).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Waiting for a scheduler slot.
    Queued,
    /// Working: calling the model or running tools.
    Running,
    /// Waiting for the user to answer an approval request.
    AwaitingApproval,
    /// Stopped at a safe point; can be resumed.
    Paused,
    /// Being repaired after a crash; becomes `Paused`.
    Recovering,
    /// Finished successfully.
    Completed,
    /// Stopped by an unrecoverable error.
    Failed,
    /// Hit its turn, token or time budget; can be resumed with a larger budget.
    BudgetExhausted,
    /// Stopped for good by the user or a parent task.
    Cancelled,
}

impl TaskState {
    /// `true` for states a task can never leave: completed, failed, cancelled.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// What a hook run decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum HookOutcome {
    /// Ran successfully and made no decision.
    Passed,
    /// Explicitly allowed the action.
    Allowed,
    /// Asked for the user to be consulted.
    AskRequested,
    /// Blocked the action (exit code 2 or a deny decision).
    Blocked {
        /// The reason given by the hook.
        reason: String,
    },
    /// Added context that was appended as a system note.
    AddedContext,
    /// Failed (non-zero exit other than 2); did not block.
    Failed {
        /// Error details.
        message: String,
    },
    /// Did not finish within its timeout.
    TimedOut,
}

/// Content that is either inline text or a reference to a blob.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BlobOrText {
    /// Inline text (small outputs).
    Text {
        /// The text.
        text: String,
    },
    /// Stored in the blob store (large outputs, over 64 KB).
    Blob {
        /// The blob.
        blob: BlobRef,
    },
}

/// Something that happened inside the core.
///
/// Streaming deltas are broadcast but not stored; see [`Event::is_persisted`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// A session was opened (created or resumed) with a persona and segment.
    SessionOpened {
        /// Active persona.
        persona: String,
        /// Current segment number.
        segment: u32,
    },
    /// User input was appended to the transcript (attachments already resolved to blobs).
    UserInput {
        /// The task the input starts or continues.
        task: TaskId,
        /// The user message exactly as it enters the transcript.
        message: Message,
    },
    /// An operator note was appended to the transcript (recall results, hook context, mode changes).
    SystemNote {
        /// The task the note belongs to.
        task: TaskId,
        /// The note text exactly as sent.
        text: String,
    },
    /// A task changed state.
    TaskState {
        /// The task.
        task: TaskId,
        /// Previous state.
        from: TaskState,
        /// New state.
        to: TaskState,
        /// Why, in a few words (for example `pause requested`, `recovered`).
        reason: String,
    },
    /// A model call started.
    TurnStarted {
        /// The task.
        task: TaskId,
        /// Turn number within the task, starting at 1.
        turn: u32,
    },
    /// Streaming text from the model (not persisted).
    TextDelta {
        /// The task.
        task: TaskId,
        /// The new text fragment.
        text: String,
    },
    /// Streaming reasoning summary from the model (not persisted; only when enabled).
    ThinkingDelta {
        /// The task.
        task: TaskId,
        /// The new text fragment.
        text: String,
    },
    /// The complete assistant message, including opaque provider blocks.
    AssistantMessage {
        /// The task.
        task: TaskId,
        /// The message exactly as it enters the transcript.
        message: Message,
    },
    /// A model call was interrupted; the partial text is kept for the record
    /// but never added to the transcript.
    AssistantAborted {
        /// The task.
        task: TaskId,
        /// Text received before the interruption.
        partial_text: String,
    },
    /// The model requested a tool call.
    ToolCall {
        /// The task.
        task: TaskId,
        /// Provider call id.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input.
        input: Value,
        /// What the call would touch.
        risk: Risk,
    },
    /// A tool call needs the user's approval.
    ApprovalRequested {
        /// The request id to answer with [`crate::Op::Approve`].
        id: ApprovalId,
        /// The task.
        task: TaskId,
        /// The tool call awaiting approval.
        call_id: String,
        /// Why approval is needed (for example the matched rule).
        reason: String,
    },
    /// An approval request was answered.
    ApprovalResolved {
        /// The request.
        id: ApprovalId,
        /// The decision.
        decision: Decision,
    },
    /// A tool (`ask_user`) asks the user a question; answer with [`crate::Op::Answer`].
    QuestionAsked {
        /// The request id to answer with.
        id: QuestionId,
        /// The task.
        task: TaskId,
        /// The tool call asking.
        call_id: String,
        /// The question text.
        question: String,
        /// Suggested answers (the user may answer in their own words).
        options: Vec<String>,
        /// Whether several options may be chosen.
        multi: bool,
    },
    /// A question was answered.
    QuestionAnswered {
        /// The request.
        id: QuestionId,
        /// The answer.
        answer: String,
    },
    /// A tool started running.
    ToolStarted {
        /// The tool call.
        call_id: String,
    },
    /// Live tool output (not persisted; the full output is in [`Event::ToolFinished`]).
    ToolOutput {
        /// The tool call.
        call_id: String,
        /// The new output fragment.
        chunk: String,
    },
    /// A tool finished; `full` keeps everything, `sent` is the capped form given to the model.
    ToolFinished {
        /// The tool call.
        call_id: String,
        /// `false` if the tool failed or was interrupted.
        ok: bool,
        /// Complete output.
        full: BlobOrText,
        /// The (possibly capped) result content as sent to the model.
        sent: Vec<ContentBlock>,
    },
    /// A hook ran.
    HookRan {
        /// Hook name.
        hook: String,
        /// Hook event name, for example `PreToolUse`.
        event: String,
        /// What it decided.
        outcome: HookOutcome,
    },
    /// Compaction of a segment started.
    CompactionStarted {
        /// The segment being compacted.
        segment: u32,
    },
    /// A segment was compacted into a new one.
    Compacted {
        /// Old segment.
        from_segment: u32,
        /// New segment.
        to_segment: u32,
        /// Number of anchors created.
        anchors: u32,
        /// Estimated context tokens before.
        before: u64,
        /// Estimated context tokens after.
        after: u64,
    },
    /// A new segment was opened with this opener message.
    SegmentOpened {
        /// The new segment number.
        segment: u32,
        /// The opener message (environment, summary, pinned facts, anchors).
        opener: Message,
    },
    /// Past content was recalled into the transcript.
    Recalled {
        /// The anchor recalled, if the recall was by anchor.
        anchor: Option<String>,
        /// Tokens added.
        tokens: u64,
        /// `true` if the harness recalled automatically, `false` if the model asked.
        auto: bool,
    },
    /// A child task was spawned.
    SubagentSpawned {
        /// Parent task.
        parent: TaskId,
        /// Child task.
        child: TaskId,
        /// Child persona.
        persona: String,
    },
    /// Files were restored by `/undo`.
    UndoApplied {
        /// The task.
        task: TaskId,
        /// Restored paths.
        paths: Vec<String>,
    },
    /// Token usage and cost of one model call.
    Usage {
        /// The task.
        task: TaskId,
        /// Token counts.
        usage: Usage,
        /// Cost in millionths of a US dollar.
        cost_micros: u64,
    },
    /// Something worth the user's attention that is not an error.
    Warning {
        /// Stable machine-readable kind.
        kind: String,
        /// Human-readable message.
        message: String,
    },
    /// An error.
    Error {
        /// Stable machine-readable kind.
        kind: String,
        /// Human-readable message.
        message: String,
        /// How to fix it, if known.
        hint: Option<String>,
    },
    /// A model call finished.
    TurnFinished {
        /// The task.
        task: TaskId,
        /// Why the model stopped.
        stop: StopReason,
    },
}

impl Event {
    /// Stable snake_case name of the variant, equal to the serialised `type` tag.
    /// Stored in the `events.kind` column.
    #[must_use]
    pub fn kind(&self) -> &'static str {
        match self {
            Self::SessionOpened { .. } => "session_opened",
            Self::UserInput { .. } => "user_input",
            Self::SystemNote { .. } => "system_note",
            Self::TaskState { .. } => "task_state",
            Self::TurnStarted { .. } => "turn_started",
            Self::TextDelta { .. } => "text_delta",
            Self::ThinkingDelta { .. } => "thinking_delta",
            Self::AssistantMessage { .. } => "assistant_message",
            Self::AssistantAborted { .. } => "assistant_aborted",
            Self::ToolCall { .. } => "tool_call",
            Self::ApprovalRequested { .. } => "approval_requested",
            Self::ApprovalResolved { .. } => "approval_resolved",
            Self::QuestionAsked { .. } => "question_asked",
            Self::QuestionAnswered { .. } => "question_answered",
            Self::ToolStarted { .. } => "tool_started",
            Self::ToolOutput { .. } => "tool_output",
            Self::ToolFinished { .. } => "tool_finished",
            Self::HookRan { .. } => "hook_ran",
            Self::CompactionStarted { .. } => "compaction_started",
            Self::Compacted { .. } => "compacted",
            Self::SegmentOpened { .. } => "segment_opened",
            Self::Recalled { .. } => "recalled",
            Self::SubagentSpawned { .. } => "subagent_spawned",
            Self::UndoApplied { .. } => "undo_applied",
            Self::Usage { .. } => "usage",
            Self::Warning { .. } => "warning",
            Self::Error { .. } => "error",
            Self::TurnFinished { .. } => "turn_finished",
        }
    }

    /// `false` for streaming deltas ([`Event::TextDelta`], [`Event::ThinkingDelta`],
    /// [`Event::ToolOutput`]), which are broadcast but never written to the log.
    #[must_use]
    pub fn is_persisted(&self) -> bool {
        !matches!(
            self,
            Self::TextDelta { .. } | Self::ThinkingDelta { .. } | Self::ToolOutput { .. }
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn samples() -> Vec<Event> {
        let t = TaskId::new();
        let msg = Message::user_text("hi");
        vec![
            Event::SessionOpened {
                persona: "coder".into(),
                segment: 0,
            },
            Event::UserInput {
                task: t,
                message: msg.clone(),
            },
            Event::SystemNote {
                task: t,
                text: "recalled a2".into(),
            },
            Event::TaskState {
                task: t,
                from: TaskState::Running,
                to: TaskState::Paused,
                reason: "pause".into(),
            },
            Event::TurnStarted { task: t, turn: 1 },
            Event::TextDelta {
                task: t,
                text: "Hel".into(),
            },
            Event::ThinkingDelta {
                task: t,
                text: "…".into(),
            },
            Event::AssistantMessage {
                task: t,
                message: msg.clone(),
            },
            Event::AssistantAborted {
                task: t,
                partial_text: "I was".into(),
            },
            Event::ToolCall {
                task: t,
                id: "c1".into(),
                name: "shell".into(),
                input: json!({"command": "ls"}),
                risk: Risk::Exec {
                    command: "ls".into(),
                },
            },
            Event::ApprovalRequested {
                id: ApprovalId::new(),
                task: t,
                call_id: "c1".into(),
                reason: "exec".into(),
            },
            Event::ApprovalResolved {
                id: ApprovalId::new(),
                decision: Decision::Once,
            },
            Event::QuestionAsked {
                id: QuestionId::new(),
                task: t,
                call_id: "c2".into(),
                question: "Which database?".into(),
                options: vec!["SQLite".into(), "Postgres".into()],
                multi: false,
            },
            Event::QuestionAnswered {
                id: QuestionId::new(),
                answer: "SQLite".into(),
            },
            Event::ToolStarted {
                call_id: "c1".into(),
            },
            Event::ToolOutput {
                call_id: "c1".into(),
                chunk: "a\n".into(),
            },
            Event::ToolFinished {
                call_id: "c1".into(),
                ok: true,
                full: BlobOrText::Blob {
                    blob: BlobRef::from_bytes([9; 32]),
                },
                sent: vec![ContentBlock::text("a")],
            },
            Event::HookRan {
                hook: "fmt".into(),
                event: "PostToolUse".into(),
                outcome: HookOutcome::Blocked { reason: "x".into() },
            },
            Event::CompactionStarted { segment: 0 },
            Event::Compacted {
                from_segment: 0,
                to_segment: 1,
                anchors: 3,
                before: 900,
                after: 90,
            },
            Event::SegmentOpened {
                segment: 1,
                opener: msg,
            },
            Event::Recalled {
                anchor: Some("a2".into()),
                tokens: 2000,
                auto: false,
            },
            Event::SubagentSpawned {
                parent: t,
                child: TaskId::new(),
                persona: "reader".into(),
            },
            Event::UndoApplied {
                task: t,
                paths: vec!["src/a.rs".into()],
            },
            Event::Usage {
                task: t,
                usage: Usage::default(),
                cost_micros: 12,
            },
            Event::Warning {
                kind: "cache_miss".into(),
                message: "m".into(),
            },
            Event::Error {
                kind: "auth".into(),
                message: "m".into(),
                hint: Some("h".into()),
            },
            Event::TurnFinished {
                task: t,
                stop: StopReason::EndTurn,
            },
        ]
    }

    #[test]
    fn every_event_round_trips_through_json() {
        for e in samples() {
            let json = serde_json::to_string(&e).unwrap();
            let back: Event = serde_json::from_str(&json).unwrap();
            assert_eq!(back, e, "{json}");
        }
    }

    #[test]
    fn kind_matches_serialised_type_tag() {
        for e in samples() {
            let v = serde_json::to_value(&e).unwrap();
            assert_eq!(v["type"], e.kind());
        }
    }

    #[test]
    fn only_streaming_deltas_are_not_persisted() {
        let not_persisted: Vec<&str> = samples()
            .iter()
            .filter(|e| !e.is_persisted())
            .map(Event::kind)
            .collect();
        assert_eq!(
            not_persisted,
            vec!["text_delta", "thinking_delta", "tool_output"]
        );
    }

    #[test]
    fn terminal_states_are_completed_failed_cancelled() {
        use TaskState::*;
        let terminal: Vec<TaskState> = [
            Queued,
            Running,
            AwaitingApproval,
            Paused,
            Recovering,
            Completed,
            Failed,
            BudgetExhausted,
            Cancelled,
        ]
        .into_iter()
        .filter(|s| s.is_terminal())
        .collect();
        assert_eq!(terminal, vec![Completed, Failed, Cancelled]);
    }

    #[test]
    fn envelope_round_trips_with_nested_event() {
        let env = Envelope {
            seq: Seq(7),
            ts: 1_763_300_000_000,
            session: SessionId::new(),
            event: Event::TurnStarted {
                task: TaskId::new(),
                turn: 2,
            },
        };
        let v = serde_json::to_value(&env).unwrap();
        assert_eq!(v["seq"], 7);
        assert_eq!(v["event"]["type"], "turn_started");
        let back: Envelope = serde_json::from_value(v).unwrap();
        assert_eq!(back, env);
    }
}
