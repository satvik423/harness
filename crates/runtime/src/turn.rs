//! The agent loop for one task: stream a reply, run its tool calls, repeat.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use futures::StreamExt;
use harness_context::{ActivePersona, SegmentContext};
use harness_ports::{
    EventSink, ProviderError, ProviderEvent, ProviderStream, RiskCtx, ToolCtx, ToolError,
};
use harness_protocol::{BlobOrText, Decision, Event, TaskState};
use harness_types::{
    ApprovalId, ContentBlock, Message, Risk, Role, SessionId, StopReason, TaskId, Usage,
};
use tokio_util::sync::CancellationToken;

use crate::approval::Approvals;
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
    pub(crate) sink: Arc<dyn EventSink>,
    pub(crate) approvals: Arc<Approvals>,
    pub(crate) cancel: CancellationToken,
    pub(crate) stop: StopSlot,
    pub(crate) state: Mutex<TaskState>,
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
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let from = *state;
        *state = to;
        drop(state);
        self.emit(Event::TaskState {
            task: self.task,
            from,
            to,
            reason: reason.into(),
        });
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

/// Runs the calls of one assistant message in order. Returns one result per
/// call (I2) and whether the task was stopped along the way.
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
    for (id, name, input) in calls {
        if stopped || ctx.cancel.is_cancelled() {
            stopped = true;
            results.push(error_result(ctx, id, "not run: the task was interrupted"));
            continue;
        }
        let Some(tool) = ctx.deps.tools.get(name) else {
            results.push(error_result(ctx, id, &format!("unknown tool {name:?}")));
            continue;
        };
        let risk = tool.risk(input, &risk_ctx);
        ctx.emit(Event::ToolCall {
            task: ctx.task,
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            risk: risk.clone(),
        });
        if !risk.is_read_only() {
            match approve(ctx, id, &risk).await {
                Approval::Granted => {}
                Approval::Denied(reason) => {
                    results.push(error_result(ctx, id, &reason));
                    continue;
                }
                Approval::Stopped => {
                    stopped = true;
                    results.push(error_result(ctx, id, "not run: the task was interrupted"));
                    continue;
                }
            }
        }
        ctx.emit(Event::ToolStarted {
            call_id: id.clone(),
        });
        let tool_ctx = ToolCtx {
            cwd: ctx.cwd.clone(),
            session: ctx.session,
            task: ctx.task,
            call_id: id.clone(),
            cancel: ctx.cancel.child_token(),
            sandbox: Arc::clone(&ctx.deps.sandbox),
            blobs: Arc::clone(&ctx.deps.blobs),
            ledger: Arc::clone(&ctx.deps.ledger),
            events: Arc::clone(&ctx.sink),
            writable_roots: risk_ctx.writable_roots.clone(),
        };
        let (content, ok) = match tool.run(input.clone(), tool_ctx).await {
            Ok(output) => (output.content, true),
            Err(e) => {
                stopped |= e == ToolError::Cancelled;
                (vec![ContentBlock::text(e.to_string())], false)
            }
        };
        ctx.emit(Event::ToolFinished {
            call_id: id.clone(),
            ok,
            full: BlobOrText::Text {
                text: text_of(&content),
            },
            sent: content.clone(),
        });
        results.push(ContentBlock::ToolResult {
            call_id: id.clone(),
            content,
            is_error: !ok,
        });
    }
    (results, stopped)
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

/// Asks for (or, under [`ApprovalMode::Never`], refuses) approval of one call.
async fn approve(ctx: &TaskCtx, call_id: &str, risk: &Risk) -> Approval {
    let action = describe(risk);
    if ctx.approval == ApprovalMode::Never {
        ctx.emit(Event::Warning {
            kind: "approval_denied".into(),
            message: format!("denied without asking: {action} (approval policy: never)"),
        });
        return Approval::Denied(
            "Denied: this session does not allow actions that need approval (approval policy: never). Do not retry; continue without it or explain what you need.".into(),
        );
    }
    let id = ApprovalId::new();
    let rx = ctx.approvals.register(id);
    ctx.set_state(TaskState::AwaitingApproval, "waiting for approval");
    ctx.emit(Event::ApprovalRequested {
        id,
        task: ctx.task,
        call_id: call_id.to_owned(),
        reason: action,
    });
    let decision = tokio::select! {
        decision = rx => decision.ok(),
        () = ctx.cancel.cancelled() => None,
    };
    ctx.approvals.forget(id);
    let Some(decision) = decision else {
        return Approval::Stopped;
    };
    ctx.emit(Event::ApprovalResolved {
        id,
        decision: decision.clone(),
    });
    ctx.set_state(TaskState::Running, "approval answered");
    match decision {
        Decision::Deny { reason } => Approval::Denied(format!(
            "The user denied this action{}",
            reason.map(|r| format!(": {r}")).unwrap_or_default()
        )),
        // Phase 1 treats every grant as one-off; remembered grants arrive with the policy engine (Phase 2).
        Decision::Once | Decision::ForSession | Decision::Always { .. } => Approval::Granted,
    }
}

/// A short human description of what a call would do.
fn describe(risk: &Risk) -> String {
    match risk {
        Risk::ReadOnly { .. } => "read files".into(),
        Risk::Write { paths } => format!(
            "write {}",
            paths
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Risk::Exec { command } => format!("run `{command}`"),
        Risk::Network { host } => format!("contact {host}"),
        Risk::External { what } => what.clone(),
    }
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
