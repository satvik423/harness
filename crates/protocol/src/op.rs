//! [`Op`]: everything a front-end can ask the core to do.

use std::path::PathBuf;

use harness_types::{ApprovalId, BlobRef, QuestionId, Seq, SessionId, TaskId};
use serde::{Deserialize, Serialize};

/// A request sent into the core by a front-end (TUI, CLI, JSON-RPC server, tests).
///
/// The session actor processes ops for one session strictly in order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Op {
    /// Open a new session in `cwd` with the given persona.
    CreateSession {
        /// Persona name, for example `coder`.
        persona: String,
        /// Project directory the session works in.
        cwd: PathBuf,
    },
    /// Re-open an existing session from the event log.
    ResumeSession {
        /// The session to resume.
        id: SessionId,
    },
    /// Add user input to a session and start (or continue) work on it.
    Submit {
        /// Target session.
        session: SessionId,
        /// Text and attachments, in order.
        input: Vec<InputItem>,
    },
    /// Stop the running model call or tool now, record it, and pause the task.
    Interrupt {
        /// Target task.
        task: TaskId,
    },
    /// Pause the task at its next safe point.
    Pause {
        /// Target task.
        task: TaskId,
    },
    /// Continue a paused task.
    Resume {
        /// Target task.
        task: TaskId,
    },
    /// Stop the task for good (also cancels its child tasks).
    Cancel {
        /// Target task.
        task: TaskId,
    },
    /// Answer a pending approval request.
    Approve {
        /// The request being answered.
        request: ApprovalId,
        /// The user's decision.
        decision: Decision,
    },
    /// Answer a pending question from the `ask_user` tool.
    Answer {
        /// The question being answered.
        request: QuestionId,
        /// The user's answer in their own words (or the chosen options).
        answer: String,
    },
    /// Compact the session's current segment now.
    Compact {
        /// Target session.
        session: SessionId,
        /// Optional focus for the summary, for example "the auth work".
        focus: Option<String>,
    },
    /// Switch persona (starts a new segment).
    SwitchPersona {
        /// Target session.
        session: SessionId,
        /// New persona name.
        persona: String,
    },
    /// Create a new session that shares history up to `at`.
    Fork {
        /// Session to fork.
        session: SessionId,
        /// Last event included in the fork.
        at: Seq,
    },
    /// Restore the files changed by the session's last turn (`/undo`).
    Undo {
        /// Target session.
        session: SessionId,
    },
    /// Shut the core down cleanly.
    Shutdown,
}

/// The user's answer to an approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "decision", rename_all = "snake_case")]
pub enum Decision {
    /// Allow this one call.
    Once,
    /// Allow matching calls for the rest of the session.
    ForSession,
    /// Allow matching calls from now on, by writing a rule.
    Always {
        /// Where the rule is saved.
        scope: RuleScope,
    },
    /// Refuse; the reason (if any) is shown to the model.
    Deny {
        /// Optional explanation for the model.
        reason: Option<String>,
    },
}

/// Where a permanent permission rule is written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    /// `~/.config/harness/rules.toml`.
    User,
    /// `.harness/rules.toml` in the project (trusted projects only).
    Project,
}

/// One item of user input.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum InputItem {
    /// Typed or pasted text.
    Text {
        /// The text.
        text: String,
    },
    /// A file path to attach (ingested into the blob store).
    File {
        /// Path to the file.
        path: PathBuf,
    },
    /// Content already in the blob store.
    Blob {
        /// The blob.
        blob: BlobRef,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn all_ops() -> Vec<Op> {
        let s = SessionId::new();
        let t = TaskId::new();
        vec![
            Op::CreateSession {
                persona: "coder".into(),
                cwd: "/tmp/p".into(),
            },
            Op::ResumeSession { id: s },
            Op::Submit {
                session: s,
                input: vec![
                    InputItem::Text {
                        text: "fix it".into(),
                    },
                    InputItem::File {
                        path: "a.pdf".into(),
                    },
                    InputItem::Blob {
                        blob: BlobRef::from_bytes([3; 32]),
                    },
                ],
            },
            Op::Interrupt { task: t },
            Op::Pause { task: t },
            Op::Resume { task: t },
            Op::Cancel { task: t },
            Op::Approve {
                request: ApprovalId::new(),
                decision: Decision::Once,
            },
            Op::Answer {
                request: QuestionId::new(),
                answer: "the second one".into(),
            },
            Op::Approve {
                request: ApprovalId::new(),
                decision: Decision::ForSession,
            },
            Op::Approve {
                request: ApprovalId::new(),
                decision: Decision::Always {
                    scope: RuleScope::Project,
                },
            },
            Op::Approve {
                request: ApprovalId::new(),
                decision: Decision::Deny {
                    reason: Some("no".into()),
                },
            },
            Op::Compact {
                session: s,
                focus: None,
            },
            Op::SwitchPersona {
                session: s,
                persona: "researcher".into(),
            },
            Op::Fork {
                session: s,
                at: Seq(42),
            },
            Op::Undo {
                session: SessionId::new(),
            },
            Op::Shutdown,
        ]
    }

    #[test]
    fn every_op_round_trips_through_json() {
        for op in all_ops() {
            let json = serde_json::to_string(&op).unwrap();
            let back: Op = serde_json::from_str(&json).unwrap();
            assert_eq!(back, op, "{json}");
        }
    }

    #[test]
    fn ops_are_tagged_with_snake_case_op_field() {
        let v = serde_json::to_value(Op::Shutdown).unwrap();
        assert_eq!(v, serde_json::json!({"op": "shutdown"}));
        let v = serde_json::to_value(Op::Pause {
            task: TaskId::new(),
        })
        .unwrap();
        assert_eq!(v["op"], "pause");
    }
}
