//! Rebuilding a session from its events (P3-T3) and crash recovery (P3-T6,
//! Tech spec 3.14).
//!
//! [`rebuild`] replays the current segment: the frozen frame from
//! `SegmentOpened`, then user input, assistant replies and tool results in
//! order. A tool round counts as answered when every call has its
//! `ToolFinished` (the runtime appends the results message only then).
//! [`recover`] repairs a task that was running when the process died.

use std::collections::{BTreeMap, BTreeSet};

use harness_context::{SegmentContext, SegmentFrame};
use harness_ports::{BlobStore, EventLog};
use harness_protocol::{BlobOrText, Event, TaskState};
use harness_types::{ContentBlock, Seq, SessionId, TaskId};

use crate::RuntimeError;
use crate::journal::Journal;
use crate::turn::Round;

/// The result given to a call that was running when the harness stopped.
pub const OUTCOME_UNKNOWN: &str = "The harness stopped while this was running. Outcome unknown. Check the current state before retrying.";

/// A session as rebuilt from its events.
pub(crate) struct Rebuilt {
    /// The current segment, if one was opened.
    pub(crate) segment: Option<SegmentContext>,
    /// The latest sequence number.
    pub(crate) last_seq: Option<Seq>,
    /// The latest task and its last recorded state.
    pub(crate) task: Option<(TaskId, TaskState)>,
    /// The unanswered tool round of the last assistant message, if any.
    pub(crate) round: Option<Round>,
    /// Calls of that round that started but never finished.
    pub(crate) started: BTreeSet<String>,
    /// Whether the latest task's input reached the log.
    pub(crate) task_input: bool,
}

fn corrupt(seq: Seq, e: impl std::fmt::Display) -> RuntimeError {
    RuntimeError::Corrupt(format!("event #{}: {e}", seq.0))
}

/// Replays a session's events.
///
/// # Errors
/// [`RuntimeError::Store`] if the log cannot be read, or
/// [`RuntimeError::Corrupt`] if the events do not form a valid transcript.
pub(crate) async fn rebuild(
    log: &dyn EventLog,
    blobs: &dyn BlobStore,
    session: SessionId,
) -> Result<Rebuilt, RuntimeError> {
    let events = log.range(session, Seq::FIRST, Seq(u64::MAX)).await?;
    let mut out = Rebuilt {
        segment: None,
        last_seq: events.last().map(|e| e.envelope.seq),
        task: None,
        round: None,
        started: BTreeSet::new(),
        task_input: false,
    };
    for logged in events {
        let seq = logged.envelope.seq;
        match logged.envelope.event {
            Event::SegmentOpened { frame, .. } => {
                let bytes = blobs.get(&frame).await?;
                let frame: SegmentFrame =
                    serde_json::from_slice(&bytes).map_err(|e| corrupt(seq, e))?;
                out.segment = Some(SegmentContext::from_frame(frame));
                out.round = None;
                out.started.clear();
            }
            Event::UserInput { task, message } => {
                segment(&mut out, seq)?
                    .replay_user(message)
                    .map_err(|e| corrupt(seq, e))?;
                if out.task.is_some_and(|(t, _)| t == task) {
                    out.task_input = true;
                }
            }
            Event::SystemNote { text, .. } => segment(&mut out, seq)?.push_note(text),
            Event::AssistantMessage { message, .. } => {
                let has_calls = !message.tool_call_ids().is_empty();
                segment(&mut out, seq)?
                    .push_assistant(message)
                    .map_err(|e| corrupt(seq, e))?;
                out.round = has_calls.then(|| Round {
                    assistant_seq: seq,
                    done: BTreeMap::new(),
                });
                out.started.clear();
            }
            Event::ToolStarted { call_id } => {
                out.started.insert(call_id);
            }
            Event::ToolFinished {
                call_id, ok, sent, ..
            } => {
                out.started.remove(&call_id);
                if let Some(round) = &mut out.round {
                    round.done.insert(
                        call_id.clone(),
                        ContentBlock::ToolResult {
                            call_id,
                            content: sent,
                            is_error: !ok,
                        },
                    );
                }
                close_round_if_answered(&mut out, seq)?;
            }
            Event::TaskState { task, to, .. } => {
                if out.task.is_none_or(|(t, _)| t != task) {
                    out.task_input = false;
                }
                out.task = Some((task, to));
            }
            _ => {}
        }
    }
    Ok(out)
}

fn segment(out: &mut Rebuilt, seq: Seq) -> Result<&mut SegmentContext, RuntimeError> {
    out.segment
        .as_mut()
        .ok_or_else(|| corrupt(seq, "a message appears before any segment was opened"))
}

/// Appends the results message once every call of the round has a result.
pub(crate) fn close_round_if_answered(out: &mut Rebuilt, seq: Seq) -> Result<(), RuntimeError> {
    let (Some(round), Some(segment)) = (&out.round, &mut out.segment) else {
        return Ok(());
    };
    let calls = segment.pending_tool_calls();
    if calls.iter().all(|(id, _, _)| round.done.contains_key(id)) {
        let results = calls
            .iter()
            .filter_map(|(id, _, _)| round.done.get(id).cloned())
            .collect();
        segment
            .push_tool_results(results)
            .map_err(|e| corrupt(seq, e))?;
        out.round = None;
    }
    Ok(())
}

/// A task left paused after rebuilding (and recovering) a session.
pub(crate) struct PausedTask {
    pub(crate) task: TaskId,
    pub(crate) round: Option<Round>,
    pub(crate) recovered: bool,
}

/// Repairs the latest task if it was running when the process stopped, and
/// returns the task left paused, if any (Tech spec 3.14):
///
/// 1. The task moves to `Recovering`.
/// 2. Each call that started but never finished gets a failed result saying
///    the outcome is unknown. It is never re-run automatically (guardrail 13).
/// 3. Calls that never started stay pending; resuming puts them back through
///    the policy, which may ask again.
/// 4. A turn whose reply was never recorded is simply asked again on resume.
/// 5. The task moves to `Paused` ("recovered"), and a warning tells the user.
///
/// # Errors
/// [`RuntimeError::Store`] if the repair cannot be recorded, or
/// [`RuntimeError::Corrupt`] if the repaired round is invalid.
pub(crate) async fn recover(
    journal: &Journal,
    out: &mut Rebuilt,
) -> Result<Option<PausedTask>, RuntimeError> {
    let Some((task, state)) = out.task else {
        return Ok(None);
    };
    match state {
        TaskState::Paused => {
            return Ok(Some(PausedTask {
                task,
                round: out.round.take(),
                recovered: false,
            }));
        }
        TaskState::Running
        | TaskState::AwaitingApproval
        | TaskState::Recovering
        | TaskState::Queued => {}
        TaskState::Completed
        | TaskState::Failed
        | TaskState::Cancelled
        | TaskState::BudgetExhausted => return Ok(None),
    }
    if !out.task_input {
        // Stopped between starting the task and saving its input: there is
        // nothing to continue.
        journal
            .record(
                Some(task),
                Event::TaskState {
                    task,
                    from: state,
                    to: TaskState::Failed,
                    reason: "harness stopped before this task's input was saved; submit it again"
                        .into(),
                },
            )
            .await?;
        return Ok(None);
    }
    journal
        .record(
            Some(task),
            Event::TaskState {
                task,
                from: state,
                to: TaskState::Recovering,
                reason: "the harness stopped while this task was running".into(),
            },
        )
        .await?;
    let unfinished: Vec<String> = out.started.iter().cloned().collect();
    for call_id in unfinished {
        let in_round = out
            .round
            .as_ref()
            .is_some_and(|r| !r.done.contains_key(&call_id));
        if !in_round {
            continue;
        }
        let sent = vec![ContentBlock::text(OUTCOME_UNKNOWN)];
        let seq = journal
            .record(
                Some(task),
                Event::ToolFinished {
                    call_id: call_id.clone(),
                    ok: false,
                    full: BlobOrText::Text {
                        text: OUTCOME_UNKNOWN.into(),
                    },
                    sent: sent.clone(),
                },
            )
            .await?;
        out.started.remove(&call_id);
        if let Some(round) = &mut out.round {
            round.done.insert(
                call_id.clone(),
                ContentBlock::ToolResult {
                    call_id,
                    content: sent,
                    is_error: true,
                },
            );
        }
        close_round_if_answered(out, seq)?;
    }
    journal
        .record(
            Some(task),
            Event::TaskState {
                task,
                from: TaskState::Recovering,
                to: TaskState::Paused,
                reason: "recovered".into(),
            },
        )
        .await?;
    journal
        .record(
            Some(task),
            Event::Warning {
                kind: "recovered".into(),
                message: format!(
                    "task {task} was interrupted when harness stopped; it is paused and can be resumed"
                ),
            },
        )
        .await?;
    Ok(Some(PausedTask {
        task,
        round: out.round.take(),
        recovered: true,
    }))
}
