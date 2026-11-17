//! The session actor and its public handle.

use std::path::PathBuf;
use std::sync::Arc;

use harness_context::{
    ActivePersona, Environment, SegmentContext, SegmentSpec, load_project_instructions,
};
use harness_ports::{BlobStore, Clock, EventSink, Provider, ReadLedger, Sandbox, ToolRegistry};
use harness_protocol::{Envelope, Event, InputItem, Op, TaskState};
use harness_types::{ContentBlock, SessionId, TaskId};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::approval::Approvals;
use crate::turn::{StopKind, StopSlot, TaskCtx, run_task};
use crate::{ApprovalMode, BroadcastSink};

/// Errors from talking to a session.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeError {
    /// The session actor has stopped.
    #[error("the session has stopped")]
    Closed,
}

/// The adapters a session uses, all behind ports.
#[derive(Clone)]
pub struct RuntimeDeps {
    /// The model provider.
    pub provider: Arc<dyn Provider>,
    /// Available tools.
    pub tools: ToolRegistry,
    /// Wall-clock time.
    pub clock: Arc<dyn Clock>,
    /// Wraps process execution.
    pub sandbox: Arc<dyn Sandbox>,
    /// Content-addressed storage.
    pub blobs: Arc<dyn BlobStore>,
    /// Read-before-edit ledger.
    pub ledger: Arc<dyn ReadLedger>,
}

/// Per-session settings.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Project directory.
    pub cwd: PathBuf,
    /// The persona for every task in this session.
    pub persona: ActivePersona,
    /// How calls needing approval are answered.
    pub approval: ApprovalMode,
    /// Maximum model calls per task.
    pub max_turns: u32,
}

/// A cloneable handle for sending ops to a session.
#[derive(Debug, Clone)]
pub struct SessionHandle {
    id: SessionId,
    ops: mpsc::Sender<Op>,
    events: broadcast::Sender<Envelope>,
}

impl SessionHandle {
    /// The session's id.
    #[must_use]
    pub fn id(&self) -> SessionId {
        self.id
    }

    /// A new receiver for the session's events (events sent earlier are not replayed).
    #[must_use]
    pub fn subscribe(&self) -> broadcast::Receiver<Envelope> {
        self.events.subscribe()
    }

    /// Sends an op to the session actor.
    ///
    /// # Errors
    /// [`RuntimeError::Closed`] if the actor has stopped.
    pub async fn send(&self, op: Op) -> Result<(), RuntimeError> {
        self.ops.send(op).await.map_err(|_| RuntimeError::Closed)
    }
}

/// Capacity of each session's op channel.
const OP_CAPACITY: usize = 64;
/// Capacity of each session's event channel; lagging subscribers miss events.
const EVENT_CAPACITY: usize = 1024;

/// Starts a session actor on the current tokio runtime.
///
/// Returns the handle and a receiver subscribed before the first event, so the
/// caller sees `SessionOpened` and everything after it.
#[must_use]
pub fn spawn_session(
    deps: RuntimeDeps,
    config: SessionConfig,
) -> (SessionHandle, broadcast::Receiver<Envelope>) {
    let id = SessionId::new();
    let (ops_tx, ops_rx) = mpsc::channel(OP_CAPACITY);
    let (events_tx, events_rx) = broadcast::channel(EVENT_CAPACITY);
    let sink: Arc<dyn EventSink> = Arc::new(BroadcastSink::new(
        id,
        Arc::clone(&deps.clock),
        events_tx.clone(),
    ));
    sink.emit(Event::SessionOpened {
        persona: config.persona.name.clone(),
        segment: 0,
    });
    let actor = Actor {
        id,
        deps,
        config,
        sink,
        ops: ops_rx,
        segment: None,
        approvals: Arc::default(),
        running: None,
    };
    tokio::spawn(actor.run());
    (
        SessionHandle {
            id,
            ops: ops_tx,
            events: events_tx,
        },
        events_rx,
    )
}

/// A task currently running on its own tokio task.
struct Running {
    task: TaskId,
    cancel: CancellationToken,
    stop: StopSlot,
    join: JoinHandle<SegmentContext>,
}

/// What woke the actor up.
enum Wake {
    Op(Option<Op>),
    TaskDone(Result<SegmentContext, tokio::task::JoinError>),
}

struct Actor {
    id: SessionId,
    deps: RuntimeDeps,
    config: SessionConfig,
    sink: Arc<dyn EventSink>,
    ops: mpsc::Receiver<Op>,
    /// The current segment while idle; moved into the running task while busy.
    segment: Option<SegmentContext>,
    approvals: Arc<Approvals>,
    running: Option<Running>,
}

impl Actor {
    async fn run(mut self) {
        loop {
            // Compute what happened first, then act on it, so the select's
            // borrows of `self` end before the handlers run.
            let wake = tokio::select! {
                op = self.ops.recv() => Wake::Op(op),
                joined = join_running(&mut self.running) => Wake::TaskDone(joined),
            };
            match wake {
                Wake::Op(None | Some(Op::Shutdown)) => {
                    self.shutdown().await;
                    break;
                }
                Wake::Op(Some(op)) => self.handle(op).await,
                Wake::TaskDone(joined) => {
                    self.running = None;
                    match joined {
                        Ok(segment) => self.segment = Some(segment),
                        Err(e) => {
                            self.warn(
                                "internal",
                                format!(
                                    "the task stopped unexpectedly ({e}); starting a fresh segment"
                                ),
                            );
                            self.segment = None;
                        }
                    }
                }
            }
        }
    }

    async fn handle(&mut self, op: Op) {
        match op {
            Op::Submit { session, input } => self.submit(session, input).await,
            Op::Approve { request, decision } => {
                if !self.approvals.resolve(request, decision) {
                    self.warn("unknown_approval", format!("no pending approval {request}"));
                }
            }
            Op::Interrupt { task } => self.stop_task(task, StopKind::Interrupt),
            Op::Cancel { task } => self.stop_task(task, StopKind::Cancel),
            Op::Pause { .. } | Op::Resume { .. } => self.warn(
                "unsupported",
                "pause and resume arrive in Phase 3; use interrupt".into(),
            ),
            Op::Compact { .. } => self.warn("unsupported", "compaction arrives in Phase 5".into()),
            Op::SwitchPersona { .. } => {
                self.warn("unsupported", "persona switching arrives in Phase 6".into())
            }
            Op::Fork { .. } | Op::CreateSession { .. } | Op::ResumeSession { .. } => {
                self.warn("unsupported", "session storage arrives in Phase 3".into());
            }
            Op::Shutdown => {}
        }
    }

    async fn submit(&mut self, session: SessionId, input: Vec<InputItem>) {
        if session != self.id {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            );
            return;
        }
        if self.running.is_some() {
            self.error(
                "busy",
                "a task is already running; wait for it, interrupt it, or cancel it".into(),
            );
            return;
        }
        let mut content = Vec::new();
        for item in input {
            match item {
                InputItem::Text { text } => content.push(ContentBlock::text(text)),
                InputItem::File { .. } | InputItem::Blob { .. } => {
                    self.error("unsupported", "attachments arrive in Phase 11".into());
                    return;
                }
            }
        }
        if content.is_empty() {
            self.error("empty_input", "nothing to send".into());
            return;
        }
        let segment = match self.segment.take() {
            Some(segment) => segment,
            None => match self.open_segment().await {
                Ok(segment) => segment,
                Err(message) => {
                    self.error("segment", message);
                    return;
                }
            },
        };
        let task = TaskId::new();
        let cancel = CancellationToken::new();
        let stop: StopSlot = Arc::default();
        let ctx = TaskCtx {
            session: self.id,
            task,
            deps: self.deps.clone(),
            cwd: self.config.cwd.clone(),
            approval: self.config.approval,
            max_turns: self.config.max_turns,
            sink: Arc::clone(&self.sink),
            approvals: Arc::clone(&self.approvals),
            cancel: cancel.clone(),
            stop: Arc::clone(&stop),
            state: std::sync::Mutex::new(TaskState::Queued),
        };
        let persona = self.config.persona.clone();
        let span = tracing::info_span!("task", session = %self.id, task = %task);
        let join = tokio::spawn(
            async move { run_task(ctx, segment, content, &persona).await }.instrument(span),
        );
        self.running = Some(Running {
            task,
            cancel,
            stop,
            join,
        });
    }

    /// Opens segment 0: gathers the environment and AGENTS.md off the async workers.
    async fn open_segment(&self) -> Result<SegmentContext, String> {
        let cwd = self.config.cwd.clone();
        let now = self.deps.clock.now_ms();
        let (environment, instructions) = tokio::task::spawn_blocking(move || {
            (
                Environment::gather(&cwd, now),
                load_project_instructions(&cwd),
            )
        })
        .await
        .map_err(|e| format!("gathering the environment failed: {e}"))?;
        let instructions = instructions.map_err(|e| e.to_string())?;
        let mut environment = environment;
        environment
            .notes
            .push(approval_note(self.config.approval).into());
        Ok(SegmentContext::open(SegmentSpec {
            persona: self.config.persona.clone(),
            tools: self.deps.tools.specs(),
            instructions: instructions.render(),
            environment: environment.render(),
            segment: 0,
        }))
    }

    fn stop_task(&self, task: TaskId, kind: StopKind) {
        match &self.running {
            Some(running) if running.task == task => {
                *running
                    .stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kind);
                running.cancel.cancel();
            }
            _ => self.warn("no_such_task", format!("task {task} is not running")),
        }
    }

    async fn shutdown(&mut self) {
        if let Some(running) = self.running.take() {
            *running
                .stop
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(StopKind::Cancel);
            running.cancel.cancel();
            // Give the task a moment to record its final state.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), running.join).await;
        }
    }

    fn warn(&self, kind: &str, message: String) {
        self.sink.emit(Event::Warning {
            kind: kind.into(),
            message,
        });
    }

    fn error(&self, kind: &str, message: String) {
        self.sink.emit(Event::Error {
            kind: kind.into(),
            message,
            hint: None,
        });
    }
}

/// Waits for the running task, or forever when idle.
async fn join_running(
    running: &mut Option<Running>,
) -> Result<SegmentContext, tokio::task::JoinError> {
    match running {
        Some(r) => (&mut r.join).await,
        None => std::future::pending().await,
    }
}

/// The environment note describing how approvals work in this session.
fn approval_note(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Ask => {
            "Commands run without a sandbox. Reads run freely; the user approves every other tool call."
        }
        ApprovalMode::Never => {
            "Commands run without a sandbox. Tool calls that need approval are denied automatically in this session."
        }
    }
}
