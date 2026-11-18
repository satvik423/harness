//! The agent loop for one task: stream a reply, run its tool calls, repeat.
//!
//! Every persisted event goes through the [`Journal`] and is committed before
//! anything that depends on it happens (NFR-03): `TurnStarted` before the model
//! call, `AssistantMessage` before its tools run, `ToolStarted` before a tool
//! starts, `ToolFinished` before the next model call.
//!
//! A pause request takes effect at a safe point (Tech spec 3.14): before each
//! model call, after each tool finishes before the next starts, and after an
//! approval decision. A running tool or model stream is never cut by a pause;
//! an interrupt does that.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use harness_context::{SegmentContext, cap_output};
use harness_ports::{
    GrantScope, PolicyCall, ProviderError, ProviderEvent, ProviderStream, RiskCtx, Tool, ToolCtx,
    ToolError, ToolOutput, Verdict,
};
use harness_protocol::{BlobOrText, Decision, Event, RuleScope, TaskState};
use harness_types::{
    ApprovalId, ContentBlock, Message, Risk, Role, Seq, SessionId, StopReason, TaskId, Usage,
};
use tokio::sync::{Notify, Semaphore};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::approval::{Approvals, Questions};
use crate::journal::Journal;
use crate::mode::SessionMode;
use crate::snapshots::TurnSnapshots;
use crate::user::{SessionUser, transition};
use crate::{ApprovalMode, RuntimeDeps};
use harness_ports::ModeControl as _;

/// Why a task was asked to stop early.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StopKind {
    /// Stop now and leave the task paused (`Op::Interrupt`).
    Interrupt,
    /// Stop for good (`Op::Cancel` or shutdown).
    Cancel,
}

/// Shared slot telling a running task why it was cancelled.
pub(crate) type StopSlot = Arc<Mutex<Option<StopKind>>>;

/// A pause request (`Op::Pause`), honoured at the next safe point.
#[derive(Debug, Default)]
pub(crate) struct PauseFlag {
    requested: AtomicBool,
    notify: Notify,
}

impl PauseFlag {
    /// Asks the task to pause at its next safe point.
    pub(crate) fn request(&self) {
        self.requested.store(true, Ordering::SeqCst);
        self.notify.notify_waiters();
    }

    fn is_requested(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    /// Resolves once a pause is requested.
    async fn wait(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        // Register before checking, so a request between the two is not missed.
        notified.as_mut().enable();
        if self.is_requested() {
            return;
        }
        notified.await;
    }
}

/// The results recorded so far for the tool calls of one assistant message.
/// Kept when a task pauses in the middle of a round, so resuming runs only the
/// calls that have no result yet.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Round {
    /// The `AssistantMessage` event whose calls these are; also the key of the
    /// files snapshotted while they run.
    pub(crate) assistant_seq: Seq,
    /// Results by call id.
    pub(crate) done: BTreeMap<String, ContentBlock>,
}

/// How a task run begins.
pub(crate) enum Start {
    /// New user input.
    New(Vec<ContentBlock>),
    /// Continue a paused task.
    Continue(Option<Round>),
    /// Cancel a paused task, closing its unanswered round.
    Cancel(Option<Round>),
}

/// What a task run leaves behind.
pub(crate) struct TaskResult {
    /// The segment, possibly extended.
    pub(crate) segment: SegmentContext,
    /// The state the task ended in.
    pub(crate) state: TaskState,
    /// The unanswered round, when it paused mid-round.
    pub(crate) round: Option<Round>,
}

/// Everything a running task needs.
pub(crate) struct TaskCtx {
    pub(crate) session: SessionId,
    pub(crate) task: TaskId,
    pub(crate) deps: RuntimeDeps,
    pub(crate) cwd: PathBuf,
    pub(crate) approval: ApprovalMode,
    pub(crate) max_turns: u32,
    pub(crate) output_cap_tokens: u64,
    pub(crate) max_parallel_tools: usize,
    pub(crate) price: Option<crate::ModelPrice>,
    pub(crate) journal: Arc<Journal>,
    pub(crate) approvals: Arc<Approvals>,
    pub(crate) questions: Arc<Questions>,
    /// The session's permission mode; read on every call, so a switch applies
    /// from the next call even while the task runs.
    pub(crate) mode: Arc<SessionMode>,
    pub(crate) cancel: CancellationToken,
    pub(crate) stop: StopSlot,
    pub(crate) pause: Arc<PauseFlag>,
    pub(crate) state: Arc<Mutex<TaskState>>,
    /// Set just before the final state is recorded: from then on the task does
    /// nothing else, so the actor may wait for it instead of calling it busy.
    pub(crate) finishing: Arc<AtomicBool>,
}

/// The event log refused a write: the task must stop before any side effect.
#[derive(Debug)]
struct LogFailed;

/// How the loop ended.
enum End {
    Completed,
    Failed(String),
    Budget,
    Stopped,
    Paused(Option<Round>),
    LogFailed,
}

/// How a tool round ended.
enum RoundEnd {
    /// Every call has a result, in call order.
    Answered(Vec<ContentBlock>),
    /// Interrupted or cancelled: unrun calls got error results, in call order.
    Stopped(Vec<ContentBlock>),
    /// Paused at a safe point; some calls have no result yet.
    Paused(Round),
}

/// The collected reply of one model call.
#[derive(Default)]
struct Reply {
    blocks: Vec<ContentBlock>,
    partial_text: String,
    usage: Usage,
    stop: Option<StopReason>,
    error: Option<ProviderError>,
}

/// Outcome of asking for approval.
enum Approval {
    Granted,
    Denied(String),
    Stopped,
    Paused,
}

impl TaskCtx {
    /// Records an event for this task.
    async fn record(&self, event: Event) -> Result<Seq, LogFailed> {
        self.journal
            .record(Some(self.task), event)
            .await
            .map_err(|_| LogFailed)
    }

    /// Records an event whose failure needs no immediate handling (the next
    /// safe point stops the task anyway).
    async fn note(&self, event: Event) {
        let _ = self.record(event).await;
    }

    /// Moves the task to `to`, recording the transition.
    async fn set_state(&self, to: TaskState, reason: impl Into<String>) {
        let _ = transition(&self.journal, self.task, &self.state, to, reason).await;
    }

    fn stop_kind(&self) -> Option<StopKind> {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner)
    }

    async fn internal_error(&self, message: String) {
        self.note(Event::Error {
            kind: "internal".into(),
            message,
            hint: None,
        })
        .await;
    }
}

/// Runs a task until it completes, fails, stops or pauses.
pub(crate) async fn run_task(
    ctx: TaskCtx,
    mut segment: SegmentContext,
    start: Start,
) -> TaskResult {
    let round = match start {
        Start::New(input) => {
            let persona = segment.frame().persona.clone();
            ctx.set_state(
                TaskState::Running,
                format!("started with persona {persona}"),
            )
            .await;
            for note in ctx.mode.take_notes() {
                segment.push_note(note);
            }
            if let Err(e) = segment.push_user(input) {
                ctx.internal_error(e.to_string()).await;
                return finish(
                    &ctx,
                    segment,
                    End::Failed("could not record the input".into()),
                )
                .await;
            }
            let message = segment.messages().last().cloned();
            if let Some(message) = message
                && ctx
                    .record(Event::UserInput {
                        task: ctx.task,
                        message,
                    })
                    .await
                    .is_err()
            {
                return finish(&ctx, segment, End::LogFailed).await;
            }
            tracing::info!(persona = %persona, "task started");
            None
        }
        Start::Continue(round) => {
            ctx.set_state(TaskState::Running, "resumed").await;
            round
        }
        Start::Cancel(round) => {
            let end = cancel_round(&ctx, &mut segment, round).await;
            return finish(&ctx, segment, end).await;
        }
    };
    let end = run_turns(&ctx, &mut segment, round).await;
    finish(&ctx, segment, end).await
}

/// Records the final state of a run.
async fn finish(ctx: &TaskCtx, segment: SegmentContext, end: End) -> TaskResult {
    let (state, reason, round) = match end {
        End::Completed => (TaskState::Completed, "done".to_owned(), None),
        End::Failed(reason) => (TaskState::Failed, reason, None),
        End::LogFailed => (
            TaskState::Failed,
            "the event log refused a write; stopped before doing anything that would not be saved"
                .to_owned(),
            None,
        ),
        End::Budget => (
            TaskState::BudgetExhausted,
            format!("reached the limit of {} model calls", ctx.max_turns),
            None,
        ),
        End::Paused(round) => (TaskState::Paused, "paused".to_owned(), round),
        End::Stopped => match ctx.stop_kind() {
            Some(StopKind::Cancel) => (TaskState::Cancelled, "cancelled".to_owned(), None),
            _ => (TaskState::Paused, "interrupted".to_owned(), None),
        },
    };
    ctx.finishing.store(true, Ordering::SeqCst);
    ctx.set_state(state, reason).await;
    TaskResult {
        segment,
        state,
        round,
    }
}

/// Answers a paused task's unanswered calls with "cancelled" so the transcript
/// stays valid, then ends the task as cancelled.
async fn cancel_round(ctx: &TaskCtx, segment: &mut SegmentContext, round: Option<Round>) -> End {
    *ctx.stop.lock().unwrap_or_else(PoisonError::into_inner) = Some(StopKind::Cancel);
    let calls = segment.pending_tool_calls();
    if calls.is_empty() {
        return End::Stopped;
    }
    let mut done = round.map(|r| r.done).unwrap_or_default();
    for (id, _, _) in &calls {
        if !done.contains_key(id) {
            match error_result(ctx, id, "not run: the task was cancelled").await {
                Ok(block) => {
                    done.insert(id.clone(), block);
                }
                Err(LogFailed) => return End::LogFailed,
            }
        }
    }
    let results = in_call_order(&calls, &mut done);
    if let Err(e) = segment.push_tool_results(results) {
        ctx.internal_error(e.to_string()).await;
        return End::Failed("could not record the tool results".into());
    }
    End::Stopped
}

async fn run_turns(ctx: &TaskCtx, segment: &mut SegmentContext, mut round: Option<Round>) -> End {
    let mut turns = 0;
    loop {
        // Finish an unanswered tool round first (a fresh reply, or a resumed task).
        let calls = segment.pending_tool_calls();
        if !calls.is_empty() {
            let current = round.take().unwrap_or_else(|| Round {
                assistant_seq: Seq(0),
                done: BTreeMap::new(),
            });
            let (results, stopped) = match run_calls(ctx, &calls, current).await {
                Ok(RoundEnd::Answered(results)) => (results, false),
                Ok(RoundEnd::Stopped(results)) => (results, true),
                Ok(RoundEnd::Paused(round)) => return End::Paused(Some(round)),
                Err(LogFailed) => return End::LogFailed,
            };
            if let Err(e) = segment.push_tool_results(results) {
                ctx.internal_error(e.to_string()).await;
                return End::Failed("could not record the tool results".into());
            }
            if stopped {
                return End::Stopped;
            }
        }
        // A resumed task whose final reply was already recorded (the harness
        // stopped just before marking it completed) has nothing left to do.
        if segment.ends_with_reply() {
            return End::Completed;
        }
        // Safe point: before each model call.
        if ctx.cancel.is_cancelled() {
            return End::Stopped;
        }
        if ctx.pause.is_requested() {
            return End::Paused(None);
        }
        if turns >= ctx.max_turns {
            return End::Budget;
        }
        turns += 1;
        if ctx
            .record(Event::TurnStarted {
                task: ctx.task,
                turn: turns,
            })
            .await
            .is_err()
        {
            return End::LogFailed;
        }
        let stream = match ctx
            .deps
            .provider
            .stream(segment.request(), ctx.cancel.child_token())
            .await
        {
            Ok(stream) => stream,
            Err(_) if ctx.cancel.is_cancelled() => return End::Stopped,
            Err(e) => return provider_failure(ctx, &e).await,
        };
        let reply = collect_reply(ctx, stream).await;
        if ctx.cancel.is_cancelled() || reply.error == Some(ProviderError::Cancelled) {
            // The partial reply is logged but never enters the transcript.
            ctx.note(Event::AssistantAborted {
                task: ctx.task,
                partial_text: reply.partial_text,
            })
            .await;
            return End::Stopped;
        }
        if let Some(e) = reply.error {
            return provider_failure(ctx, &e).await;
        }
        ctx.note(Event::Usage {
            task: ctx.task,
            usage: reply.usage,
            cost_micros: ctx.price.map_or(0, |p| p.cost_micros(&reply.usage)),
        })
        .await;
        let stop = reply.stop.unwrap_or_else(|| StopReason::Other {
            raw: "stream ended without a stop reason".into(),
        });

        if !reply.blocks.is_empty() {
            let message = Message::new(Role::Assistant, reply.blocks);
            if let Err(e) = segment.push_assistant(message.clone()) {
                ctx.internal_error(e.to_string()).await;
                return End::Failed("could not record the reply".into());
            }
            let has_calls = !message.tool_call_ids().is_empty();
            let Ok(seq) = ctx
                .record(Event::AssistantMessage {
                    task: ctx.task,
                    message,
                })
                .await
            else {
                return End::LogFailed;
            };
            round = has_calls.then(|| Round {
                assistant_seq: seq,
                done: BTreeMap::new(),
            });
        }
        ctx.note(Event::TurnFinished {
            task: ctx.task,
            stop: stop.clone(),
        })
        .await;

        let pending = !segment.pending_tool_calls().is_empty();
        match &stop {
            StopReason::Refusal { category } => {
                return End::Failed(format!(
                    "the model declined the request{}",
                    category
                        .as_ref()
                        .map(|c| format!(" ({c})"))
                        .unwrap_or_default()
                ));
            }
            StopReason::MaxTokens if !pending => {
                return End::Failed("the reply hit the output limit before finishing".into());
            }
            // Only server-side tools pause a turn, and none are used yet. Continuing
            // would mean sending an assistant-final request, so end the task instead.
            StopReason::PauseTurn if !pending => {
                ctx.note(Event::Warning {
                    kind: "pause_turn".into(),
                    message: "the provider paused the turn; continuing paused turns arrives with server-side tools".into(),
                })
                .await;
                return End::Completed;
            }
            _ => {}
        }
        if !pending {
            return End::Completed;
        }
    }
}

/// Reads the provider stream to its end, broadcasting live deltas.
async fn collect_reply(ctx: &TaskCtx, mut stream: ProviderStream) -> Reply {
    let mut reply = Reply::default();
    loop {
        let item = tokio::select! {
            item = stream.next() => item,
            () = ctx.cancel.cancelled() => {
                reply.error = Some(ProviderError::Cancelled);
                break;
            }
        };
        match item {
            None => break,
            Some(Ok(ProviderEvent::TextDelta { text })) => {
                reply.partial_text.push_str(&text);
                ctx.journal.live(Event::TextDelta {
                    task: ctx.task,
                    text,
                });
            }
            Some(Ok(ProviderEvent::ThinkingDelta { text })) => {
                ctx.journal.live(Event::ThinkingDelta {
                    task: ctx.task,
                    text,
                });
            }
            Some(Ok(
                ProviderEvent::ToolCallStart { .. } | ProviderEvent::ToolCallInputDelta { .. },
            )) => {}
            Some(Ok(ProviderEvent::BlockComplete { block })) => reply.blocks.push(block),
            Some(Ok(ProviderEvent::Usage { usage })) => reply.usage = usage,
            Some(Ok(ProviderEvent::Stop { reason })) => reply.stop = Some(reason),
            Some(Ok(ProviderEvent::Notice { message })) => {
                ctx.note(Event::Warning {
                    kind: "provider_notice".into(),
                    message,
                })
                .await;
            }
            Some(Err(e)) => {
                reply.error = Some(e);
                break;
            }
        }
    }
    reply
}

/// How the policy and the user ruled on one call, before it runs.
enum Assessed {
    /// Run it; `parallel` if it may run alongside other parallel-safe calls.
    Run { tool: Arc<dyn Tool>, parallel: bool },
    /// Do not run it; report this message to the model as an error result.
    /// `true` if it was refused only because nobody can approve it.
    Refused(String, bool),
    /// The policy wants the user's approval first.
    Ask {
        tool: Arc<dyn Tool>,
        call: PolicyCall,
        reason: String,
    },
}

/// Judges one call with the policy. Pure: records nothing and asks nobody.
fn assess(
    ctx: &TaskCtx,
    risk_ctx: &RiskCtx,
    name: &str,
    input: &serde_json::Value,
) -> (Assessed, Option<Risk>) {
    let Some(tool) = ctx.deps.tools.get(name) else {
        return (
            Assessed::Refused(format!("unknown tool {name:?}"), false),
            None,
        );
    };
    let risk = tool.risk(input, risk_ctx);
    let call = PolicyCall {
        tool: name.to_owned(),
        input: input.clone(),
        risk: risk.clone(),
        cwd: risk_ctx.cwd.clone(),
        writable_roots: risk_ctx.writable_roots.clone(),
        mode: ctx.mode.current(),
    };
    let assessed = match ctx.deps.policy.evaluate(&call) {
        Verdict::Allow { reason } => {
            tracing::debug!(tool = name, %reason, "allowed by policy");
            Assessed::Run {
                parallel: tool.parallel_safe() && risk.is_read_only(),
                tool,
            }
        }
        Verdict::Deny {
            reason,
            approval_required,
        } => Assessed::Refused(reason, approval_required),
        Verdict::Ask { reason } => Assessed::Ask { tool, call, reason },
    };
    (assessed, Some(risk))
}

/// Takes the results of `calls` out of `done`, in call order.
fn in_call_order(
    calls: &[(String, String, serde_json::Value)],
    done: &mut BTreeMap<String, ContentBlock>,
) -> Vec<ContentBlock> {
    calls
        .iter()
        .filter_map(|(id, _, _)| done.remove(id))
        .collect()
}

/// Runs the calls of one assistant message that have no result yet.
///
/// Calls are judged and approved one at a time, in order. A run of consecutive
/// parallel-safe, allowed calls executes together (at most `max_parallel_tools`
/// at once); every other call runs on its own. Results always keep call order (I2).
async fn run_calls(
    ctx: &TaskCtx,
    calls: &[(String, String, serde_json::Value)],
    round: Round,
) -> Result<RoundEnd, LogFailed> {
    let risk_ctx = RiskCtx {
        cwd: ctx.cwd.clone(),
        writable_roots: vec![ctx.cwd.clone()],
    };
    let turn = round.assistant_seq;
    let mut done = round.done;
    let mut stopped = false;
    let mut i = 0;
    while i < calls.len() {
        let (id, name, input) = &calls[i];
        if done.contains_key(id) {
            i += 1;
            continue;
        }
        if stopped || ctx.cancel.is_cancelled() {
            stopped = true;
            let block = error_result(ctx, id, "not run: the task was interrupted").await?;
            done.insert(id.clone(), block);
            i += 1;
            continue;
        }
        // Safe point: after a tool finishes, before the next one starts.
        if ctx.pause.is_requested() {
            return Ok(RoundEnd::Paused(Round {
                assistant_seq: turn,
                done,
            }));
        }
        let (assessed, risk) = assess(ctx, &risk_ctx, name, input);
        if let Some(risk) = &risk {
            ctx.record(Event::ToolCall {
                task: ctx.task,
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                risk: risk.clone(),
            })
            .await?;
        }
        match assessed {
            Assessed::Refused(reason, approval_required) => {
                if risk.is_some() {
                    // `approval_denied` tells headless front-ends to exit with code 4.
                    let kind = if approval_required {
                        "approval_denied"
                    } else {
                        "policy_denied"
                    };
                    ctx.note(Event::Warning {
                        kind: kind.into(),
                        message: format!("{name}: {reason}"),
                    })
                    .await;
                }
                let block = error_result(ctx, id, &reason).await?;
                done.insert(id.clone(), block);
                i += 1;
            }
            Assessed::Ask { tool, call, reason } => {
                match approve(ctx, id, &call, &reason).await? {
                    Approval::Granted => {
                        // Safe point: after an approval decision.
                        if ctx.pause.is_requested() {
                            return Ok(RoundEnd::Paused(Round {
                                assistant_seq: turn,
                                done,
                            }));
                        }
                        let (block, cancelled) =
                            run_one(ctx, &risk_ctx, tool, id, input, turn).await?;
                        stopped |= cancelled;
                        done.insert(id.clone(), block);
                    }
                    Approval::Denied(reason) => {
                        let block = error_result(ctx, id, &reason).await?;
                        done.insert(id.clone(), block);
                    }
                    Approval::Stopped => {
                        stopped = true;
                        let block =
                            error_result(ctx, id, "not run: the task was interrupted").await?;
                        done.insert(id.clone(), block);
                    }
                    Approval::Paused => {
                        return Ok(RoundEnd::Paused(Round {
                            assistant_seq: turn,
                            done,
                        }));
                    }
                }
                i += 1;
            }
            Assessed::Run {
                tool,
                parallel: false,
            } => {
                let (block, cancelled) = run_one(ctx, &risk_ctx, tool, id, input, turn).await?;
                stopped |= cancelled;
                done.insert(id.clone(), block);
                i += 1;
            }
            Assessed::Run {
                tool,
                parallel: true,
            } => {
                // Gather the following unanswered calls that may also run in parallel.
                let mut batch = vec![(i, tool)];
                let mut next = i + 1;
                while let Some((next_id, next_name, next_input)) = calls.get(next) {
                    if done.contains_key(next_id) {
                        next += 1;
                        continue;
                    }
                    let (
                        Assessed::Run {
                            tool,
                            parallel: true,
                        },
                        Some(risk),
                    ) = assess(ctx, &risk_ctx, next_name, next_input)
                    else {
                        break;
                    };
                    ctx.record(Event::ToolCall {
                        task: ctx.task,
                        id: next_id.clone(),
                        name: next_name.clone(),
                        input: next_input.clone(),
                        risk,
                    })
                    .await?;
                    batch.push((next, tool));
                    next += 1;
                }
                let (blocks, cancelled) = run_batch(ctx, &risk_ctx, calls, batch, turn).await?;
                stopped |= cancelled;
                for (call_id, block) in blocks {
                    done.insert(call_id, block);
                }
                i = next;
            }
        }
    }
    let results = in_call_order(calls, &mut done);
    Ok(if stopped {
        RoundEnd::Stopped(results)
    } else {
        RoundEnd::Answered(results)
    })
}

/// Builds the context for one call.
fn tool_ctx(ctx: &TaskCtx, risk_ctx: &RiskCtx, id: &str, turn: Seq) -> ToolCtx {
    ToolCtx {
        cwd: ctx.cwd.clone(),
        session: ctx.session,
        task: ctx.task,
        call_id: id.to_owned(),
        cancel: ctx.cancel.child_token(),
        sandbox: Arc::clone(&ctx.deps.sandbox),
        blobs: Arc::clone(&ctx.deps.blobs),
        ledger: Arc::clone(&ctx.deps.ledger),
        events: Arc::clone(&ctx.journal) as _,
        writable_roots: risk_ctx.writable_roots.clone(),
        user: Arc::new(SessionUser {
            task: ctx.task,
            call_id: id.to_owned(),
            mode: ctx.approval,
            journal: Arc::clone(&ctx.journal),
            questions: Arc::clone(&ctx.questions),
            state: Arc::clone(&ctx.state),
        }),
        snapshots: Arc::new(TurnSnapshots {
            session: ctx.session,
            turn,
            blobs: Arc::clone(&ctx.deps.blobs),
            store: Arc::clone(&ctx.deps.snapshots),
        }),
        mode: Arc::clone(&ctx.mode) as _,
    }
}

/// Turns a tool's outcome into result content: (content, ok, interrupted).
fn outcome(result: Result<ToolOutput, ToolError>) -> (Vec<ContentBlock>, bool, bool) {
    match result {
        Ok(output) => (output.content, true, false),
        Err(e) => {
            let cancelled = e == ToolError::Cancelled;
            (vec![ContentBlock::text(e.to_string())], false, cancelled)
        }
    }
}

/// Runs one call and records its result; `true` if it was interrupted.
async fn run_one(
    ctx: &TaskCtx,
    risk_ctx: &RiskCtx,
    tool: Arc<dyn Tool>,
    id: &str,
    input: &serde_json::Value,
    turn: Seq,
) -> Result<(ContentBlock, bool), LogFailed> {
    // Committed before the tool runs, so recovery knows it may have run.
    ctx.record(Event::ToolStarted {
        call_id: id.to_owned(),
    })
    .await?;
    let cap = tool.output_cap_tokens().unwrap_or(ctx.output_cap_tokens);
    let (content, ok, cancelled) = outcome(
        tool.run(input.clone(), tool_ctx(ctx, risk_ctx, id, turn))
            .await,
    );
    Ok((record_result(ctx, id, content, ok, cap).await?, cancelled))
}

/// Runs parallel-safe calls together, at most `max_parallel_tools` at once,
/// and records their results in call order.
async fn run_batch(
    ctx: &TaskCtx,
    risk_ctx: &RiskCtx,
    calls: &[(String, String, serde_json::Value)],
    batch: Vec<(usize, Arc<dyn Tool>)>,
    turn: Seq,
) -> Result<(Vec<(String, ContentBlock)>, bool), LogFailed> {
    let limit = Arc::new(Semaphore::new(ctx.max_parallel_tools.max(1)));
    let mut set = JoinSet::new();
    let mut caps = Vec::with_capacity(batch.len());
    for (slot, (index, tool)) in batch.into_iter().enumerate() {
        let (id, _, input) = &calls[index];
        ctx.record(Event::ToolStarted {
            call_id: id.clone(),
        })
        .await?;
        caps.push((
            id.clone(),
            tool.output_cap_tokens().unwrap_or(ctx.output_cap_tokens),
        ));
        let tool_ctx = tool_ctx(ctx, risk_ctx, id, turn);
        let input = input.clone();
        let limit = Arc::clone(&limit);
        set.spawn(async move {
            // The semaphore is never closed, so acquiring only waits for a slot.
            let _permit = limit.acquire_owned().await;
            (slot, tool.run(input, tool_ctx).await)
        });
    }
    let mut finished: Vec<Option<Result<ToolOutput, ToolError>>> =
        (0..caps.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        match joined {
            Ok((slot, result)) => finished[slot] = Some(result),
            Err(e) => tracing::error!(error = %e, "a parallel tool call panicked"),
        }
    }
    let mut blocks = Vec::with_capacity(caps.len());
    let mut cancelled = false;
    for ((id, cap), result) in caps.into_iter().zip(finished) {
        let result = result
            .unwrap_or_else(|| Err(ToolError::Failed("the tool stopped unexpectedly".into())));
        let (content, ok, was_cancelled) = outcome(result);
        cancelled |= was_cancelled;
        let block = record_result(ctx, &id, content, ok, cap).await?;
        blocks.push((id, block));
    }
    Ok((blocks, cancelled))
}

/// An error result for a call that did not run, recorded as a failed tool.
async fn error_result(ctx: &TaskCtx, id: &str, message: &str) -> Result<ContentBlock, LogFailed> {
    let content = vec![ContentBlock::text(message)];
    ctx.record(Event::ToolFinished {
        call_id: id.to_owned(),
        ok: false,
        full: BlobOrText::Text {
            text: message.to_owned(),
        },
        sent: content.clone(),
    })
    .await?;
    Ok(ContentBlock::ToolResult {
        call_id: id.to_owned(),
        content,
        is_error: true,
    })
}

/// Full outputs larger than this are stored as a blob instead of inline text.
const INLINE_FULL_MAX_BYTES: usize = 64 * 1024;

/// Records a finished call and returns the result block for the transcript.
///
/// This is the one place output is capped (FR-TOOL-04): the capped form is
/// decided here, stored in the event as `sent`, and never changes afterwards.
/// The footer points at this event's own sequence number, where `full` keeps
/// the complete output.
async fn record_result(
    ctx: &TaskCtx,
    id: &str,
    content: Vec<ContentBlock>,
    ok: bool,
    cap_tokens: u64,
) -> Result<ContentBlock, LogFailed> {
    let text = text_of(&content);
    let full = if text.len() > INLINE_FULL_MAX_BYTES {
        match ctx
            .deps
            .blobs
            .put(text.clone().into_bytes(), "text/plain; charset=utf-8", None)
            .await
        {
            Ok(blob) => BlobOrText::Blob { blob },
            Err(e) => {
                // Keeping the text inline is worse for the log but loses nothing.
                tracing::warn!(error = %e, "could not store tool output as a blob");
                BlobOrText::Text { text }
            }
        }
    } else {
        BlobOrText::Text { text }
    };
    let for_event = content.clone();
    let call_id = id.to_owned();
    let seq = ctx
        .journal
        .record_with(Some(ctx.task), move |seq| Event::ToolFinished {
            call_id,
            ok,
            full,
            sent: cap_output(&for_event, cap_tokens, seq),
        })
        .await
        .map_err(|_| LogFailed)?;
    // `cap_output` is deterministic: this is exactly what the event holds.
    Ok(ContentBlock::ToolResult {
        call_id: id.to_owned(),
        content: cap_output(&content, cap_tokens, seq),
        is_error: !ok,
    })
}

/// Longest approval summary kept, in characters.
const MAX_SUMMARY_CHARS: usize = 120;

/// The call's `description` input on one line, for the approval panel (ADR-043).
///
/// Any tool's `description` string counts, so MCP tools that ask for one get it
/// too. Squeezed and cut here so every front-end shows the same line.
fn summary_of(input: &serde_json::Value) -> Option<String> {
    let line = input
        .get("description")?
        .as_str()?
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    if line.is_empty() {
        return None;
    }
    Some(if line.chars().count() > MAX_SUMMARY_CHARS {
        let cut: String = line.chars().take(MAX_SUMMARY_CHARS - 1).collect();
        format!("{cut}…")
    } else {
        line
    })
}

/// Asks the user about one call (or refuses under [`ApprovalMode::Never`]), and
/// remembers session and permanent grants with the policy.
async fn approve(
    ctx: &TaskCtx,
    call_id: &str,
    call: &PolicyCall,
    reason: &str,
) -> Result<Approval, LogFailed> {
    if ctx.approval == ApprovalMode::Never {
        ctx.note(Event::Warning {
            kind: "approval_denied".into(),
            message: format!("denied without asking: {reason} (approval policy: never)"),
        })
        .await;
        return Ok(Approval::Denied(format!(
            "Denied: {reason} needs approval, and this session does not ask (approval policy: never). Do not retry; continue without it or explain what you need."
        )));
    }
    let id = ApprovalId::new();
    let rx = ctx.approvals.register(id);
    ctx.set_state(TaskState::AwaitingApproval, "waiting for approval")
        .await;
    ctx.record(Event::ApprovalRequested {
        id,
        task: ctx.task,
        call_id: call_id.to_owned(),
        reason: reason.to_owned(),
        summary: summary_of(&call.input),
    })
    .await?;
    let decision = tokio::select! {
        decision = rx => decision.ok(),
        () = ctx.cancel.cancelled() => None,
        () = ctx.pause.wait() => {
            ctx.approvals.forget(&id);
            // The call stays unanswered and goes through the policy again on resume.
            return Ok(Approval::Paused);
        }
    };
    ctx.approvals.forget(&id);
    let Some(decision) = decision else {
        return Ok(Approval::Stopped);
    };
    ctx.record(Event::ApprovalResolved {
        id,
        decision: decision.clone(),
    })
    .await?;
    ctx.set_state(TaskState::Running, "approval answered").await;
    let scope = match decision {
        Decision::Deny { reason } => {
            return Ok(Approval::Denied(format!(
                "The user denied this action{}",
                reason.map(|r| format!(": {r}")).unwrap_or_default()
            )));
        }
        Decision::Once => return Ok(Approval::Granted),
        Decision::ForSession => GrantScope::Session,
        Decision::Always {
            scope: RuleScope::User,
        } => GrantScope::User,
        Decision::Always {
            scope: RuleScope::Project,
        } => GrantScope::Project,
    };
    // Saving a rule may write a file: keep it off the async workers.
    let policy = Arc::clone(&ctx.deps.policy);
    let owned = call.clone();
    let remembered = tokio::task::spawn_blocking(move || policy.remember(&owned, scope)).await;
    let failure = match remembered {
        Ok(Ok(what)) => {
            tracing::info!(%what, "approval remembered");
            None
        }
        Ok(Err(e)) => Some(e.to_string()),
        Err(e) => Some(e.to_string()),
    };
    if let Some(e) = failure {
        ctx.note(Event::Warning {
            kind: "approval_not_remembered".into(),
            message: format!("approved this once only: {e}"),
        })
        .await;
    }
    Ok(Approval::Granted)
}

/// Concatenated text of result content.
fn text_of(content: &[ContentBlock]) -> String {
    content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// Reports a provider error and fails the task.
async fn provider_failure(ctx: &TaskCtx, e: &ProviderError) -> End {
    let (kind, hint) = match e {
        ProviderError::Auth(_) => (
            "provider_auth",
            Some(
                "Check the API key: `secret-tool store --label='harness anthropic' service harness username anthropic`, or set ANTHROPIC_API_KEY.",
            ),
        ),
        ProviderError::RateLimited { .. } => (
            "provider_rate_limited",
            Some("Wait a moment and try again."),
        ),
        ProviderError::Overloaded { .. } => (
            "provider_overloaded",
            Some("The provider is busy; try again shortly."),
        ),
        ProviderError::Server { .. } => ("provider_server", None),
        ProviderError::Network(_) => ("provider_network", Some("Check your internet connection.")),
        ProviderError::Timeout => ("provider_timeout", None),
        ProviderError::InvalidRequest { .. } => ("provider_invalid_request", None),
        ProviderError::Decode(_) => ("provider_decode", None),
        ProviderError::Cancelled => ("cancelled", None),
        ProviderError::Unsupported(_) => ("provider_unsupported", None),
    };
    ctx.note(Event::Error {
        kind: kind.into(),
        message: e.to_string(),
        hint: hint.map(str::to_owned),
    })
    .await;
    End::Failed(format!("provider error: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn summaries_are_one_trimmed_line_cut_at_the_limit() {
        assert_eq!(summary_of(&json!({"command": "ls"})), None);
        assert_eq!(summary_of(&json!({"description": " \n "})), None);
        assert_eq!(summary_of(&json!({"description": 3})), None);
        let long = summary_of(&json!({"description": "word ".repeat(60)})).unwrap();
        assert_eq!(long.chars().count(), MAX_SUMMARY_CHARS);
        assert!(long.ends_with('…'));
    }
}
