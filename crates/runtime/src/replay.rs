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
use harness_ports::{BlobStore, EventLog, ModelRequest};
use harness_protocol::{BlobOrText, Event, TaskState};
use harness_types::{ContentBlock, Mode, Seq, SessionId, TaskId};

use crate::RuntimeError;
use crate::journal::Journal;
use crate::memory::SessionMemory;
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
    /// The permission mode in force at the end of the log, if it says.
    pub(crate) mode: Option<Mode>,
    /// Pins and anchors from the session's compactions.
    pub(crate) memory: SessionMemory,
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
    replay(log, blobs, session, &mut |_, _| {}).await
}

/// One model request rebuilt from the log.
#[derive(Debug, Clone, PartialEq)]
pub struct RecordedRequest {
    /// The `TurnStarted` event it was sent at.
    pub seq: Seq,
    /// The segment it belongs to: the append-only check compares requests of
    /// one segment only (a compaction starts a new transcript).
    pub segment: u32,
    /// The request exactly as the model received it.
    pub request: ModelRequest,
}

/// The model request sent at each turn of the session, every segment,
/// rebuilt from the log (for `harness debug last-request` and
/// `diff-requests`). Compaction requests are not included.
///
/// # Errors
/// As for resuming: [`RuntimeError::Store`] or [`RuntimeError::Corrupt`].
pub async fn request_history(
    log: &dyn EventLog,
    blobs: &dyn BlobStore,
    session: SessionId,
) -> Result<Vec<RecordedRequest>, RuntimeError> {
    let mut requests = Vec::new();
    replay(log, blobs, session, &mut |seq, segment| {
        requests.push(RecordedRequest {
            seq,
            segment: segment.segment(),
            request: segment.request(),
        });
    })
    .await?;
    Ok(requests)
}

/// Replays the log, calling `on_turn` with the segment as it stood at each
/// `TurnStarted` (that is the request the model received).
async fn replay(
    log: &dyn EventLog,
    blobs: &dyn BlobStore,
    session: SessionId,
    on_turn: &mut (dyn FnMut(Seq, &SegmentContext) + Send),
) -> Result<Rebuilt, RuntimeError> {
    let events = log.range(session, Seq::FIRST, Seq(u64::MAX)).await?;
    let mut out = Rebuilt {
        segment: None,
        last_seq: events.last().map(|e| e.envelope.seq),
        task: None,
        round: None,
        started: BTreeSet::new(),
        task_input: false,
        mode: None,
        memory: SessionMemory::default(),
    };
    for logged in events {
        let seq = logged.envelope.seq;
        match logged.envelope.event {
            Event::SegmentOpened { frame, .. } => {
                let bytes = blobs.get(&frame).await?;
                let frame: SegmentFrame =
                    serde_json::from_slice(&bytes).map_err(|e| corrupt(seq, e))?;
                // Notes not yet sent move to the new segment, as they do live.
                let carried: Vec<String> = out
                    .segment
                    .as_ref()
                    .map(|s| s.pending_notes().to_vec())
                    .unwrap_or_default();
                let mut next = SegmentContext::from_frame(frame);
                for note in carried {
                    next.push_note(note);
                }
                out.segment = Some(next);
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
            Event::SessionOpened { mode, .. } => out.mode = mode.or(out.mode),
            Event::Compacted { record, .. } => out.memory.apply(&record),
            // The same measurement the live loop takes before appending a reply.
            Event::Usage { usage, .. } => {
                if let Some(segment) = &mut out.segment {
                    segment.record_input(usage.total_input());
                }
            }
            // Live, the note waits for the next user message only when a segment
            // is open; a new segment's environment states the mode instead.
            Event::ModeChanged { to, note, .. } => {
                out.mode = Some(to);
                if let Some(segment) = &mut out.segment {
                    segment.push_note(note);
                }
            }
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
            Event::TurnStarted { .. } => {
                if let Some(segment) = &out.segment {
                    on_turn(seq, segment);
                }
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
