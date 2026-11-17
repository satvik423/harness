//! The agent loop for one task: stream a reply, run its tool calls, repeat.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use harness_context::{ActivePersona, SegmentContext, cap_output};
use harness_ports::{
    EventSink, GrantScope, PolicyCall, ProviderError, ProviderEvent, ProviderStream, RiskCtx,
    ToolCtx, ToolError, Verdict,
};
use harness_protocol::{BlobOrText, Decision, Event, RuleScope, TaskState};
use harness_types::{
    ApprovalId, ContentBlock, Message, Risk, Role, SessionId, StopReason, TaskId, Usage,
};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::approval::{Approvals, Questions};
use crate::user::{SessionUser, transition};
use crate::{ApprovalMode, RuntimeDeps};

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
    pub(crate) sink: Arc<dyn EventSink>,
    pub(crate) approvals: Arc<Approvals>,
    pub(crate) questions: Arc<Questions>,
    pub(crate) cancel: CancellationToken,
    pub(crate) stop: StopSlot,
    pub(crate) state: Arc<Mutex<TaskState>>,
}

/// How the loop ended.
enum End {
    Completed,
    Failed(String),
    Budget,
    Stopped,
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
}

impl TaskCtx {
    fn emit(&self, event: Event) {
        self.sink.emit(event);
    }

    /// Moves the task to `to`, emitting the transition from the current state.
    fn set_state(&self, to: TaskState, reason: impl Into<String>) {
        transition(self.sink.as_ref(), self.task, &self.state, to, reason);
    }

    fn stop_kind(&self) -> Option<StopKind> {
        *self.stop.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// Runs one task to its end and returns the (possibly extended) segment.
pub(crate) async fn run_task(
    ctx: TaskCtx,
    mut segment: SegmentContext,
    input: Vec<ContentBlock>,
    persona: &ActivePersona,
) -> SegmentContext {
    ctx.set_state(
        TaskState::Running,
        format!("started with persona {}", persona.name),
    );
    if let Err(e) = segment.push_user(input) {
        ctx.emit(Event::Error {
            kind: "internal".into(),
            message: e.to_string(),
            hint: None,
        });
        ctx.set_state(TaskState::Failed, "could not record the input");
        return segment;
    }
    if let Some(message) = segment.messages().last() {
        ctx.emit(Event::UserInput {
            task: ctx.task,
            message: message.clone(),
        });
    }
    tracing::info!(persona = %persona.name, model = %persona.model, "task started");
    let end = run_turns(&ctx, &mut segment).await;
    match end {
        End::Completed => ctx.set_state(TaskState::Completed, "done"),
        End::Failed(reason) => ctx.set_state(TaskState::Failed, reason),
        End::Budget => ctx.set_state(
            TaskState::BudgetExhausted,
            format!("reached the limit of {} model calls", ctx.max_turns),
        ),
        End::Stopped => match ctx.stop_kind() {
            Some(StopKind::Cancel) => ctx.set_state(TaskState::Cancelled, "cancelled"),
            _ => ctx.set_state(TaskState::Paused, "interrupted"),
        },
    }
    segment
}

async fn run_turns(ctx: &TaskCtx, segment: &mut SegmentContext) -> End {
    for turn in 1..=ctx.max_turns {
        ctx.emit(Event::TurnStarted {
            task: ctx.task,
            turn,
        });
        let stream = match ctx
            .deps
            .provider
            .stream(segment.request(), ctx.cancel.child_token())
            .await
        {
            Ok(stream) => stream,
            Err(_) if ctx.cancel.is_cancelled() => return End::Stopped,
            Err(e) => return provider_failure(ctx, &e),
        };
        let reply = collect_reply(ctx, stream).await;
        if ctx.cancel.is_cancelled() || reply.error == Some(ProviderError::Cancelled) {
            ctx.emit(Event::AssistantAborted {
                task: ctx.task,
                partial_text: reply.partial_text,
            });
            return End::Stopped;
        }
        if let Some(e) = reply.error {
            return provider_failure(ctx, &e);
        }
        ctx.emit(Event::Usage {
            task: ctx.task,
            usage: reply.usage,
            cost_micros: 0,
        });
        let stop = reply.stop.unwrap_or_else(|| StopReason::Other {
            raw: "stream ended without a stop reason".into(),
        });

        if !reply.blocks.is_empty() {
            let message = Message::new(Role::Assistant, reply.blocks);
            if let Err(e) = segment.push_assistant(message.clone()) {
                ctx.emit(Event::Error {
                    kind: "internal".into(),
                    message: e.to_string(),
                    hint: None,
                });
                return End::Failed("could not record the reply".into());
            }
            ctx.emit(Event::AssistantMessage {
                task: ctx.task,
                message,
            });
        }
        ctx.emit(Event::TurnFinished {
            task: ctx.task,
            stop: stop.clone(),
        });

        let calls = segment.pending_tool_calls();
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
            StopReason::MaxTokens if calls.is_empty() => {
                return End::Failed("the reply hit the output limit before finishing".into());
            }
            // Only server-side tools pause a turn, and none are used yet. Continuing
            // would mean sending an assistant-final request, so end the task instead.
            StopReason::PauseTurn if calls.is_empty() => {
                ctx.emit(Event::Warning {
                    kind: "pause_turn".into(),
                    message: "the provider paused the turn; continuing paused turns arrives with server-side tools".into(),
                });
                return End::Completed;
            }
            _ => {}
        }
        if calls.is_empty() {
            return End::Completed;
        }
        let (results, stopped) = run_calls(ctx, &calls).await;
        if let Err(e) = segment.push_tool_results(results) {
            ctx.emit(Event::Error {
                kind: "internal".into(),
                message: e.to_string(),
                hint: None,
            });
            return End::Failed("could not record the tool results".into());
        }
        if stopped {
            return End::Stopped;
        }
    }
    End::Budget
}

/// Reads the provider stream to its end, emitting live deltas.
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
                ctx.emit(Event::TextDelta {
                    task: ctx.task,
                    text,
                });
            }
            Some(Ok(ProviderEvent::ThinkingDelta { text })) => ctx.emit(Event::ThinkingDelta {
                task: ctx.task,
                text,
            }),
            Some(Ok(
                ProviderEvent::ToolCallStart { .. } | ProviderEvent::ToolCallInputDelta { .. },
            )) => {}
            Some(Ok(ProviderEvent::BlockComplete { block })) => reply.blocks.push(block),
            Some(Ok(ProviderEvent::Usage { usage })) => reply.usage = usage,
            Some(Ok(ProviderEvent::Stop { reason })) => reply.stop = Some(reason),
            Some(Ok(ProviderEvent::Notice { message })) => {
                ctx.emit(Event::Warning {
                    kind: "provider_notice".into(),
                    message,
                });
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
    Run {
        tool: Arc<dyn harness_ports::Tool>,
        parallel: bool,
    },
    /// Do not run it; report this message to the model as an error result.
    /// `true` if it was refused only because nobody can approve it.
    Refused(String, bool),
    /// The policy wants the user's approval first.
    Ask {
        tool: Arc<dyn harness_ports::Tool>,
        call: PolicyCall,
        reason: String,
    },
}

/// Judges one call with the policy. Pure: emits nothing and asks nobody.
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

/// Runs the calls of one assistant message. Returns one result per call, in
/// call order (I2), and whether the task was stopped along the way.
///
/// Calls are judged and approved one at a time, in order. A run of consecutive
/// parallel-safe, allowed calls executes together (at most
/// `max_parallel_tools` at once); every other call runs on its own.
async fn run_calls(
    ctx: &TaskCtx,
    calls: &[(String, String, serde_json::Value)],
) -> (Vec<ContentBlock>, bool) {
    let risk_ctx = RiskCtx {
        cwd: ctx.cwd.clone(),
        writable_roots: vec![ctx.cwd.clone()],
    };
    let mut results = Vec::with_capacity(calls.len());
    let mut stopped = false;
    let mut i = 0;
    while i < calls.len() {
        let (id, name, input) = &calls[i];
        if stopped || ctx.cancel.is_cancelled() {
            stopped = true;
            results.push(error_result(ctx, id, "not run: the task was interrupted"));
            i += 1;
            continue;
        }
        let (assessed, risk) = assess(ctx, &risk_ctx, name, input);
        if let Some(risk) = &risk {
            ctx.emit(Event::ToolCall {
                task: ctx.task,
                id: id.clone(),
                name: name.clone(),
                input: input.clone(),
                risk: risk.clone(),
            });
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
                    ctx.emit(Event::Warning {
                        kind: kind.into(),
                        message: format!("{name}: {reason}"),
                    });
                }
                results.push(error_result(ctx, id, &reason));
                i += 1;
            }
            Assessed::Ask { tool, call, reason } => {
                match approve(ctx, id, &call, &reason).await {
                    Approval::Granted => {
                        let (block, cancelled) = run_one(ctx, &risk_ctx, tool, id, input).await;
                        stopped |= cancelled;
                        results.push(block);
                    }
                    Approval::Denied(reason) => results.push(error_result(ctx, id, &reason)),
                    Approval::Stopped => {
                        stopped = true;
                        results.push(error_result(ctx, id, "not run: the task was interrupted"));
                    }
                }
                i += 1;
            }
            Assessed::Run {
                tool,
                parallel: false,
            } => {
                let (block, cancelled) = run_one(ctx, &risk_ctx, tool, id, input).await;
                stopped |= cancelled;
                results.push(block);
                i += 1;
            }
            Assessed::Run {
                tool,
                parallel: true,
            } => {
                // Gather the following calls that may also run in parallel.
                let mut batch = vec![(i, tool)];
                while let Some((next_id, next_name, next_input)) = calls.get(i + batch.len()) {
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
                    ctx.emit(Event::ToolCall {
                        task: ctx.task,
                        id: next_id.clone(),
                        name: next_name.clone(),
                        input: next_input.clone(),
                        risk,
                    });
                    batch.push((i + batch.len(), tool));
                }
                let n = batch.len();
                let (blocks, cancelled) = run_batch(ctx, &risk_ctx, calls, batch).await;
                stopped |= cancelled;
                results.extend(blocks);
                i += n;
            }
        }
    }
    (results, stopped)
}

/// Builds the context for one call.
fn tool_ctx(ctx: &TaskCtx, risk_ctx: &RiskCtx, id: &str) -> ToolCtx {
    ToolCtx {
        cwd: ctx.cwd.clone(),
        session: ctx.session,
        task: ctx.task,
        call_id: id.to_owned(),
        cancel: ctx.cancel.child_token(),
        sandbox: Arc::clone(&ctx.deps.sandbox),
        blobs: Arc::clone(&ctx.deps.blobs),
        ledger: Arc::clone(&ctx.deps.ledger),
        events: Arc::clone(&ctx.sink),
        writable_roots: risk_ctx.writable_roots.clone(),
        user: Arc::new(SessionUser {
            task: ctx.task,
            call_id: id.to_owned(),
            mode: ctx.approval,
            sink: Arc::clone(&ctx.sink),
            questions: Arc::clone(&ctx.questions),
            state: Arc::clone(&ctx.state),
        }),
    }
}

/// Turns a tool's outcome into result content; `true` if it was interrupted.
fn outcome(
    result: Result<harness_ports::ToolOutput, ToolError>,
) -> (Vec<ContentBlock>, bool, bool) {
    match result {
        Ok(output) => (output.content, true, false),
        Err(e) => {
            let cancelled = e == ToolError::Cancelled;
            (vec![ContentBlock::text(e.to_string())], false, cancelled)
        }
    }
}

/// Runs one call and records its result.
async fn run_one(
    ctx: &TaskCtx,
    risk_ctx: &RiskCtx,
    tool: Arc<dyn harness_ports::Tool>,
    id: &str,
    input: &serde_json::Value,
) -> (ContentBlock, bool) {
    ctx.emit(Event::ToolStarted {
        call_id: id.to_owned(),
    });
    let cap = tool.output_cap_tokens().unwrap_or(ctx.output_cap_tokens);
    let (content, ok, cancelled) =
        outcome(tool.run(input.clone(), tool_ctx(ctx, risk_ctx, id)).await);
    (record_result(ctx, id, content, ok, cap).await, cancelled)
}

/// Runs parallel-safe calls together, at most `max_parallel_tools` at once,
/// and records their results in call order.
async fn run_batch(
    ctx: &TaskCtx,
    risk_ctx: &RiskCtx,
    calls: &[(String, String, serde_json::Value)],
    batch: Vec<(usize, Arc<dyn harness_ports::Tool>)>,
) -> (Vec<ContentBlock>, bool) {
    let limit = Arc::new(Semaphore::new(ctx.max_parallel_tools.max(1)));
    let mut set = JoinSet::new();
    let mut caps = Vec::with_capacity(batch.len());
    for (slot, (index, tool)) in batch.into_iter().enumerate() {
        let (id, _, input) = &calls[index];
        ctx.emit(Event::ToolStarted {
            call_id: id.clone(),
        });
        caps.push((
            id.clone(),
            tool.output_cap_tokens().unwrap_or(ctx.output_cap_tokens),
        ));
        let tool_ctx = tool_ctx(ctx, risk_ctx, id);
        let input = input.clone();
        let limit = Arc::clone(&limit);
        set.spawn(async move {
            // The semaphore is never closed, so acquiring only waits for a slot.
            let _permit = limit.acquire_owned().await;
            (slot, tool.run(input, tool_ctx).await)
        });
    }
    let mut finished: Vec<Option<Result<harness_ports::ToolOutput, ToolError>>> =
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
        blocks.push(record_result(ctx, &id, content, ok, cap).await);
    }
    (blocks, cancelled)
}

/// An error result for a call that did not run, reported to the UI as a failed tool.
fn error_result(ctx: &TaskCtx, id: &str, message: &str) -> ContentBlock {
    let content = vec![ContentBlock::text(message)];
    ctx.emit(Event::ToolFinished {
        call_id: id.to_owned(),
        ok: false,
        full: BlobOrText::Text {
            text: message.to_owned(),
        },
        sent: content.clone(),
    });
    ContentBlock::ToolResult {
        call_id: id.to_owned(),
        content,
        is_error: true,
    }
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
) -> ContentBlock {
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
    let mut sent = Vec::new();
    let mut full = Some(full);
    ctx.sink.emit_with(&mut |seq| {
        sent = cap_output(&content, cap_tokens, seq);
        Event::ToolFinished {
            call_id: id.to_owned(),
            ok,
            full: full.take().unwrap_or(BlobOrText::Text {
                text: String::new(),
            }),
            sent: sent.clone(),
        }
    });
    ContentBlock::ToolResult {
        call_id: id.to_owned(),
        content: sent,
        is_error: !ok,
    }
}

/// Asks the user about one call (or refuses under [`ApprovalMode::Never`]), and
/// remembers session and permanent grants with the policy.
async fn approve(ctx: &TaskCtx, call_id: &str, call: &PolicyCall, reason: &str) -> Approval {
    if ctx.approval == ApprovalMode::Never {
        ctx.emit(Event::Warning {
            kind: "approval_denied".into(),
            message: format!("denied without asking: {reason} (approval policy: never)"),
        });
        return Approval::Denied(format!(
            "Denied: {reason} needs approval, and this session does not ask (approval policy: never). Do not retry; continue without it or explain what you need."
        ));
    }
    let id = ApprovalId::new();
    let rx = ctx.approvals.register(id);
    ctx.set_state(TaskState::AwaitingApproval, "waiting for approval");
    ctx.emit(Event::ApprovalRequested {
        id,
        task: ctx.task,
        call_id: call_id.to_owned(),
        reason: reason.to_owned(),
    });
    let decision = tokio::select! {
        decision = rx => decision.ok(),
        () = ctx.cancel.cancelled() => None,
    };
    ctx.approvals.forget(&id);
    let Some(decision) = decision else {
        return Approval::Stopped;
    };
    ctx.emit(Event::ApprovalResolved {
        id,
        decision: decision.clone(),
    });
    ctx.set_state(TaskState::Running, "approval answered");
    let scope = match decision {
        Decision::Deny { reason } => {
            return Approval::Denied(format!(
                "The user denied this action{}",
                reason.map(|r| format!(": {r}")).unwrap_or_default()
            ));
        }
        Decision::Once => return Approval::Granted,
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
    match remembered {
        Ok(Ok(what)) => tracing::info!(%what, "approval remembered"),
        Ok(Err(e)) => ctx.emit(Event::Warning {
            kind: "approval_not_remembered".into(),
            message: format!("approved this once only: {e}"),
        }),
        Err(e) => ctx.emit(Event::Warning {
            kind: "approval_not_remembered".into(),
            message: format!("approved this once only: {e}"),
        }),
    }
    Approval::Granted
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
fn provider_failure(ctx: &TaskCtx, e: &ProviderError) -> End {
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
    ctx.emit(Event::Error {
        kind: kind.into(),
        message: e.to_string(),
        hint: hint.map(str::to_owned),
    });
    End::Failed(format!("provider error: {e}"))
}
