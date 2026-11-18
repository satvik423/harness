//! The session actor and its public handle.
//!
//! One actor owns a session: its segment, its event numbering (through the
//! [`Journal`]) and its one active task. A task is either running on its own
//! tokio task, or paused (kept here with its unanswered tool round) until it is
//! resumed or cancelled. While a task is active the actor also checks the
//! store for control requests from other processes (`harness task pause ID`).

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use harness_context::{
    ActivePersona, Environment, SegmentContext, SegmentSpec, load_project_instructions,
};
use harness_ports::{
    BlobStore, Clock, EventLog, LoggedEvent, ModeControl as _, Policy, Provider, ReadLedger,
    Sandbox, SessionRecord, SessionStatus, SessionStore, SnapshotStore, StoreError, TaskControl,
    ToolRegistry,
};
use harness_protocol::{Envelope, Event, InputItem, Op, TaskState};
use harness_types::{ContentBlock, Effort, Message, Mode, ModelRef, Seq, SessionId, TaskId};
use thiserror::Error;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;
use tracing::Instrument;

use crate::approval::{Approvals, Questions};
use crate::compact::FrameChange;
use crate::journal::Journal;
use crate::memory::{ContextSettings, SessionMemory};
use crate::mode::{SessionMode, environment_note};
use crate::replay::{PausedTask, rebuild, recover};
use crate::turn::{PauseFlag, Round, Start, StopKind, StopSlot, TaskCtx, TaskResult, run_task};
use crate::{ApprovalMode, ModelPrice};

/// Errors from starting or talking to a session.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeError {
    /// The session actor has stopped.
    #[error("the session has stopped")]
    Closed,
    /// No session with this id is stored.
    #[error("no session {0} in this project")]
    NotFound(SessionId),
    /// The store failed.
    #[error(transparent)]
    Store(#[from] StoreError),
    /// The stored events do not form a valid session.
    #[error("the stored session is inconsistent: {0}")]
    Corrupt(String),
    /// A request that cannot be carried out as given (for example a fork
    /// point after the session's last event).
    #[error("{0}")]
    Invalid(String),
}

/// A new fork, started (see [`fork_session`]).
#[derive(Debug)]
pub struct Forked {
    /// The fork's handle.
    pub handle: SessionHandle,
    /// Its events, subscribed before the first one.
    pub events: broadcast::Receiver<Envelope>,
    /// The parent event the fork continues after (moved back to before an
    /// unanswered tool round if one was asked for).
    pub at: Seq,
    /// Whether the fork holds conversation to compact: send `Op::Compact`
    /// next, so it starts its own segment (ADR-047).
    pub needs_compaction: bool,
}

/// How a session gets a provider and price for another model, for `/model`
/// inside a session (ADR-046). Implemented by the front-end, which knows the
/// keys and the pricing table; it may block (keyring), so the runtime calls it
/// off the async workers.
pub trait ModelSource: Send + Sync {
    /// A provider that serves `model`, and the model's price if known.
    ///
    /// # Errors
    /// A message when the model cannot be used (unknown provider, no API key).
    fn open(&self, model: &ModelRef) -> Result<(Arc<dyn Provider>, Option<ModelPrice>), String>;
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
    /// Decides allow, ask or deny for every tool call.
    pub policy: Arc<dyn Policy>,
    /// The append-only event log (source of truth).
    pub log: Arc<dyn EventLog>,
    /// The session catalogue and task-control mailbox.
    pub sessions: Arc<dyn SessionStore>,
    /// File snapshots for undo.
    pub snapshots: Arc<dyn SnapshotStore>,
    /// Providers for other models (`None`: only models of the same provider,
    /// at the session's price).
    pub models: Option<Arc<dyn ModelSource>>,
}

/// Per-session settings.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    /// Project directory.
    pub cwd: PathBuf,
    /// The persona for new segments in this session.
    pub persona: ActivePersona,
    /// How calls needing approval are answered.
    pub approval: ApprovalMode,
    /// The permission mode a new session starts in (ADR-042). A resumed
    /// session keeps the mode it had.
    pub mode: Mode,
    /// Whether `full` mode may be chosen (`--dangerously-full-access`).
    pub full_access: bool,
    /// Context-engine settings: when and how to compact (Phase 5).
    pub context: ContextSettings,
    /// Maximum model calls per task run.
    pub max_turns: u32,
    /// Tool output cap in estimated tokens, unless a tool sets its own
    /// (`context.output_cap_tokens`, default 2,000).
    pub output_cap_tokens: u64,
    /// Parallel-safe calls run at once, at most (`runtime.max_parallel_tools`).
    pub max_parallel_tools: usize,
    /// The session model's price, for `Usage` costs (`None`: costs are 0).
    pub price: Option<ModelPrice>,
}

/// What resuming a session found.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resumed {
    /// The session's paused task, if it has one; send `Op::Resume` to continue it.
    pub paused: Option<TaskId>,
    /// `true` if that task was running when the harness last stopped and has
    /// just been recovered (Tech spec 3.14).
    pub recovered: bool,
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

    /// Waits until the session actor has stopped (after [`Op::Shutdown`]).
    /// Every event it recorded is committed by then.
    pub async fn closed(&self) {
        self.ops.closed().await;
    }
}

/// Capacity of each session's op channel.
const OP_CAPACITY: usize = 64;
/// Capacity of each session's event channel; lagging subscribers miss events.
const EVENT_CAPACITY: usize = 1024;
/// How often an active task's session checks for control requests.
const CONTROL_POLL: Duration = Duration::from_millis(250);

/// Starts a new session: registers it in the store, records `SessionOpened`,
/// and starts its actor on the current tokio runtime.
///
/// Returns the handle and a receiver subscribed before the first event.
///
/// # Errors
/// [`RuntimeError::Store`] if the session cannot be registered.
pub async fn spawn_session(
    deps: RuntimeDeps,
    config: SessionConfig,
) -> Result<(SessionHandle, broadcast::Receiver<Envelope>), RuntimeError> {
    let id = SessionId::new();
    deps.sessions
        .create_session(SessionRecord {
            id,
            created_at: deps.clock.now_ms(),
            title: None,
            persona: config.persona.name.clone(),
            cwd: config.cwd.clone(),
            status: SessionStatus::Open,
            parent: None,
            forked_at: None,
            current_segment: 0,
        })
        .await?;
    let (events_tx, events_rx) = broadcast::channel(EVENT_CAPACITY);
    let journal = Arc::new(Journal::new(
        id,
        Arc::clone(&deps.log),
        Arc::clone(&deps.clock),
        events_tx.clone(),
        None,
        0,
    ));
    journal
        .record(
            None,
            Event::SessionOpened {
                persona: config.persona.name.clone(),
                segment: 0,
                mode: Some(config.mode),
            },
        )
        .await?;
    let mode = config.mode;
    let handle = start(
        Actor::new(id, deps, config, journal, Restored::fresh(mode)),
        events_tx,
    );
    Ok((handle, events_rx))
}

/// Opens a stored session: rebuilds it from its events, recovers a task that
/// was running when the harness stopped, and starts its actor.
///
/// The receiver is subscribed before recovery, so it sees the recovery events.
///
/// # Errors
/// [`RuntimeError::NotFound`] for an unknown session, [`RuntimeError::Corrupt`]
/// if its events are inconsistent, or [`RuntimeError::Store`].
pub async fn resume_session(
    deps: RuntimeDeps,
    config: SessionConfig,
    id: SessionId,
) -> Result<(SessionHandle, broadcast::Receiver<Envelope>, Resumed), RuntimeError> {
    if deps.sessions.session(id).await?.is_none() {
        return Err(RuntimeError::NotFound(id));
    }
    let mut rebuilt = rebuild(deps.log.as_ref(), deps.blobs.as_ref(), id).await?;
    let (events_tx, events_rx) = broadcast::channel(EVENT_CAPACITY);
    let segment_no = rebuilt.segment.as_ref().map_or(0, SegmentContext::segment);
    let journal = Arc::new(Journal::new(
        id,
        Arc::clone(&deps.log),
        Arc::clone(&deps.clock),
        events_tx.clone(),
        rebuilt.last_seq,
        segment_no,
    ));
    let paused = recover(&journal, &mut rebuilt).await?;
    let resumed = Resumed {
        paused: paused.as_ref().map(|p| p.task),
        recovered: paused.as_ref().is_some_and(|p| p.recovered),
    };
    let mode = rebuilt.mode.unwrap_or(config.mode);
    let full_access = config.full_access;
    let memory = std::mem::take(&mut rebuilt.memory);
    let actor = Actor::new(
        id,
        deps,
        config,
        journal,
        Restored {
            segment: rebuilt.segment,
            paused,
            mode,
            memory,
        },
    );
    // A session left in full mode does not keep it in a process started
    // without --dangerously-full-access.
    if mode == Mode::Full && !full_access {
        let _ = actor.mode.set(Mode::Auto).await;
        actor
            .warn(
                "mode",
                "this session was in full mode; it continues in auto mode because harness was started without --dangerously-full-access".into(),
            )
            .await;
    }
    Ok((start(actor, events_tx), events_rx, resumed))
}

/// Forks `parent` after its event `at` (FR-SES-06, ADR-035, ADR-047): a new
/// session that reads the parent's events up to `at` in place and records its
/// own after them.
///
/// The fork must not reuse the parent's thinking blocks, so it continues in a
/// segment of its own: when the inherited segment holds conversation, the
/// caller sends `Op::Compact` and the fork starts from a summary of the
/// parent's segment up to `at`. A fork point inside an unanswered tool round
/// moves back to before that round (I4).
///
/// # Errors
/// [`RuntimeError::NotFound`] for an unknown parent, [`RuntimeError::Invalid`]
/// for a fork point outside it, or a store error.
pub async fn fork_session(
    deps: RuntimeDeps,
    config: SessionConfig,
    parent: SessionId,
    at: Seq,
) -> Result<Forked, RuntimeError> {
    let Some(record) = deps.sessions.session(parent).await? else {
        return Err(RuntimeError::NotFound(parent));
    };
    let last = deps.log.last_seq(parent).await?.unwrap_or(Seq(0));
    if at.0 == 0 || at > last {
        return Err(RuntimeError::Invalid(format!(
            "event #{} is not in session {parent} (its events are #1 to #{})",
            at.0, last.0
        )));
    }
    let history = deps.log.range(parent, Seq::FIRST, at).await?;
    let at = safe_fork_point(&history, at);
    let segment = history
        .iter()
        .take_while(|l| l.envelope.seq <= at)
        .last()
        .map_or(0, |l| l.segment);
    let id = SessionId::new();
    deps.sessions
        .create_session(SessionRecord {
            id,
            created_at: deps.clock.now_ms(),
            title: record.title.map(|t| format!("{t} (fork at #{})", at.0)),
            persona: record.persona,
            cwd: config.cwd.clone(),
            status: SessionStatus::Open,
            parent: Some(parent),
            forked_at: Some(at),
            current_segment: segment,
        })
        .await?;
    let mut rebuilt = rebuild(deps.log.as_ref(), deps.blobs.as_ref(), id).await?;
    let (events_tx, events_rx) = broadcast::channel(EVENT_CAPACITY);
    let journal = Arc::new(Journal::new(
        id,
        Arc::clone(&deps.log),
        Arc::clone(&deps.clock),
        events_tx.clone(),
        Some(at),
        segment,
    ));
    let mut mode = rebuilt.mode.unwrap_or(config.mode);
    if mode == Mode::Full && !config.full_access {
        mode = Mode::Auto;
    }
    journal
        .record(
            None,
            Event::SessionOpened {
                persona: config.persona.name.clone(),
                segment,
                mode: Some(mode),
            },
        )
        .await?;
    let needs_compaction = rebuilt
        .segment
        .as_ref()
        .is_some_and(|s| !s.messages().is_empty());
    let memory = std::mem::take(&mut rebuilt.memory);
    let actor = Actor::new(
        id,
        deps,
        config,
        journal,
        Restored {
            segment: rebuilt.segment,
            paused: None,
            mode,
            memory,
        },
    );
    Ok(Forked {
        handle: start(actor, events_tx),
        events: events_rx,
        at,
        needs_compaction,
    })
}

/// `at`, or the event before the assistant message whose tool round `at`
/// falls inside (that round has calls without results up to `at`).
fn safe_fork_point(history: &[LoggedEvent], at: Seq) -> Seq {
    let mut open: Option<(Seq, usize)> = None;
    for logged in history.iter().take_while(|l| l.envelope.seq <= at) {
        match &logged.envelope.event {
            Event::AssistantMessage { message, .. } => {
                let calls = message.tool_call_ids().len();
                open = (calls > 0).then_some((logged.envelope.seq, calls));
            }
            Event::ToolFinished { .. } => {
                if let Some((seq, left)) = open {
                    open = (left > 1).then_some((seq, left - 1));
                }
            }
            _ => {}
        }
    }
    match open {
        Some((assistant, _)) => Seq(assistant.0.saturating_sub(1).max(1)),
        None => at,
    }
}

fn start(actor: Actor, events: broadcast::Sender<Envelope>) -> SessionHandle {
    let id = actor.id;
    let (ops_tx, ops_rx) = mpsc::channel(OP_CAPACITY);
    tokio::spawn(actor.run(ops_rx));
    SessionHandle {
        id,
        ops: ops_tx,
        events,
    }
}

/// What a session starts with besides its settings: nothing for a new
/// session, the rebuilt state for a resumed one or a fork.
struct Restored {
    segment: Option<SegmentContext>,
    paused: Option<PausedTask>,
    mode: Mode,
    memory: SessionMemory,
}

impl Restored {
    /// A new session in `mode`.
    fn fresh(mode: Mode) -> Self {
        Self {
            segment: None,
            paused: None,
            mode,
            memory: SessionMemory::default(),
        }
    }
}

/// A model switch in progress.
struct Switch {
    model: ModelRef,
    effort: Option<Effort>,
    provider: Arc<dyn Provider>,
    price: Option<ModelPrice>,
}

/// A task currently running on its own tokio task.
struct Running {
    task: TaskId,
    cancel: CancellationToken,
    stop: StopSlot,
    pause: Arc<PauseFlag>,
    finishing: Arc<std::sync::atomic::AtomicBool>,
    join: JoinHandle<TaskResult>,
}

/// A paused task waiting for `Op::Resume` or `Op::Cancel`.
struct Paused {
    task: TaskId,
    round: Option<Round>,
}

/// What woke the actor up.
enum Wake {
    Op(Option<Op>),
    TaskDone(Result<TaskResult, tokio::task::JoinError>),
    Tick,
}

struct Actor {
    id: SessionId,
    deps: RuntimeDeps,
    config: SessionConfig,
    journal: Arc<Journal>,
    /// The current segment while no task runs; moved into the running task.
    segment: Option<SegmentContext>,
    approvals: Arc<Approvals>,
    questions: Arc<Questions>,
    /// The permission mode, shared with running tasks and their tools.
    mode: Arc<SessionMode>,
    /// Pins and anchors from earlier compactions.
    memory: Arc<std::sync::Mutex<SessionMemory>>,
    /// A model switch waiting for its compaction to finish.
    switching: Option<Switch>,
    running: Option<Running>,
    paused: Option<Paused>,
    /// Parent of every task's cancellation token.
    root: CancellationToken,
}

impl Actor {
    fn new(
        id: SessionId,
        deps: RuntimeDeps,
        config: SessionConfig,
        journal: Arc<Journal>,
        restored: Restored,
    ) -> Self {
        let Restored {
            segment,
            paused,
            mode,
            memory,
        } = restored;
        let mode = Arc::new(SessionMode::new(
            mode,
            config.full_access,
            Arc::clone(&journal),
        ));
        Self {
            id,
            deps,
            config,
            journal,
            segment,
            approvals: Arc::default(),
            questions: Arc::default(),
            mode,
            memory: Arc::new(std::sync::Mutex::new(memory)),
            switching: None,
            running: None,
            paused: paused.map(|p| Paused {
                task: p.task,
                round: p.round,
            }),
            root: CancellationToken::new(),
        }
    }

    async fn run(mut self, mut ops: mpsc::Receiver<Op>) {
        let mut tick = tokio::time::interval(CONTROL_POLL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            // Compute what happened first, then act on it, so the select's
            // borrows of `self` end before the handlers run.
            let active = self.running.is_some() || self.paused.is_some();
            let wake = tokio::select! {
                op = ops.recv() => Wake::Op(op),
                joined = join_running(&mut self.running) => Wake::TaskDone(joined),
                _ = tick.tick(), if active => Wake::Tick,
            };
            match wake {
                Wake::Op(None | Some(Op::Shutdown)) => {
                    self.shutdown().await;
                    break;
                }
                Wake::Op(Some(op)) => self.handle(op).await,
                Wake::TaskDone(joined) => {
                    if let Some(running) = self.running.take() {
                        self.task_done(running.task, joined).await;
                    }
                }
                Wake::Tick => self.poll_controls().await,
            }
        }
    }

    async fn task_done(
        &mut self,
        task: TaskId,
        joined: Result<TaskResult, tokio::task::JoinError>,
    ) {
        match joined {
            Ok(result) => {
                if let Some(switch) = self.switching.take() {
                    let frame = result.segment.frame();
                    if frame.provider == switch.model.provider()
                        && frame.model == switch.model.model()
                        && frame.effort == switch.effort
                    {
                        self.apply_switch(switch).await;
                    } else {
                        self.warn(
                            "model",
                            format!("the session stays on {}/{}: the compaction for the switch did not finish", frame.provider, frame.model),
                        )
                        .await;
                    }
                }
                if result.state == TaskState::Paused {
                    self.paused = Some(Paused {
                        task,
                        round: result.round,
                    });
                }
                self.segment = Some(result.segment);
            }
            Err(e) => {
                self.warn(
                    "internal",
                    format!("the task stopped unexpectedly ({e}); starting a fresh segment"),
                )
                .await;
                self.segment = None;
            }
        }
    }

    /// Collects a task that has already finished but whose end the actor has
    /// not processed yet: it records its final state just before its handle
    /// completes, so an op can arrive in between.
    async fn reap_finished(&mut self) {
        let finished = |r: &Running| {
            r.join.is_finished() || r.finishing.load(std::sync::atomic::Ordering::SeqCst)
        };
        if self.running.as_ref().is_some_and(finished)
            && let Some(running) = self.running.take()
        {
            let joined = running.join.await;
            self.task_done(running.task, joined).await;
        }
    }

    async fn handle(&mut self, op: Op) {
        self.reap_finished().await;
        match op {
            Op::Submit { session, input } => self.submit(session, input).await,
            Op::Approve { request, decision } => {
                if !self.approvals.resolve(&request, decision) {
                    self.warn("unknown_approval", format!("no pending approval {request}"))
                        .await;
                }
            }
            Op::Answer { request, answer } => {
                if !self.questions.resolve(&request, answer) {
                    self.warn("unknown_question", format!("no pending question {request}"))
                        .await;
                }
            }
            Op::Interrupt { task } => self.stop_task(task, StopKind::Interrupt).await,
            Op::Cancel { task } => self.cancel(task).await,
            Op::Pause { task } => self.pause(task).await,
            Op::Resume { task } => self.resume(task).await,
            Op::Undo { session } => self.undo(session).await,
            Op::SetMode { session, mode } => self.set_mode(session, mode).await,
            Op::SwitchModel {
                session,
                model,
                effort,
            } => self.switch_model(session, model, effort).await,
            Op::Compact { session, focus } => {
                self.compact(session, focus, FrameChange::default()).await;
            }
            Op::SwitchPersona { .. } => {
                self.warn("unsupported", "persona switching arrives in Phase 6".into())
                    .await;
            }
            Op::Fork { .. } => {
                self.warn(
                    "unsupported",
                    "a fork is a new session: create it through the front-end (harness session fork, /fork)".into(),
                )
                .await;
            }
            Op::CreateSession { .. } | Op::ResumeSession { .. } => {
                self.warn(
                    "unsupported",
                    "one actor runs one session; create or resume sessions through the front-end"
                        .into(),
                )
                .await;
            }
            Op::Shutdown => {}
        }
    }

    async fn submit(&mut self, session: SessionId, input: Vec<InputItem>) {
        if session != self.id {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            )
            .await;
            return;
        }
        if self.running.is_some() {
            self.error(
                "busy",
                "a task is already running; wait for it, interrupt it, or cancel it".into(),
            )
            .await;
            return;
        }
        let mut content = Vec::new();
        for item in input {
            match item {
                InputItem::Text { text } => content.push(ContentBlock::text(text)),
                InputItem::File { path } => match attachment_note(&path).await {
                    Ok(note) => content.push(ContentBlock::text(note)),
                    Err(message) => {
                        self.error("attachment", message).await;
                        return;
                    }
                },
                InputItem::Blob { .. } => {
                    self.error("unsupported", "blob attachments arrive in Phase 11".into())
                        .await;
                    return;
                }
            }
        }
        if content.is_empty() {
            self.error("empty_input", "nothing to send".into()).await;
            return;
        }
        // New input supersedes a paused task (typically one just interrupted):
        // close it as cancelled, answering any unanswered calls, then start anew.
        if let Some(paused) = self.paused.take()
            && let Some(segment) = self.segment.take()
        {
            self.start_task(paused.task, segment, Start::Cancel(paused.round));
            if let Some(running) = self.running.take() {
                let joined = running.join.await;
                self.task_done(running.task, joined).await;
                if self.segment.is_none() {
                    return;
                }
            }
        }
        let segment = match self.segment.take() {
            Some(segment) => segment,
            None => match self.open_segment().await {
                Ok(segment) => segment,
                Err(message) => {
                    self.error("segment", message).await;
                    return;
                }
            },
        };
        self.start_task(TaskId::new(), segment, Start::New(content));
    }

    fn start_task(&mut self, task: TaskId, segment: SegmentContext, start: Start) {
        let cancel = self.root.child_token();
        let stop: StopSlot = Arc::default();
        let pause: Arc<PauseFlag> = Arc::default();
        let finishing: Arc<std::sync::atomic::AtomicBool> = Arc::default();
        let initial = match start {
            Start::New(_) => TaskState::Queued,
            Start::Continue(_) | Start::Cancel(_) => TaskState::Paused,
            Start::Compact { .. } => TaskState::Queued,
        };
        let ctx = TaskCtx {
            session: self.id,
            task,
            deps: self.deps.clone(),
            cwd: self.config.cwd.clone(),
            approval: self.config.approval,
            max_turns: self.config.max_turns,
            output_cap_tokens: self.config.output_cap_tokens,
            max_parallel_tools: self.config.max_parallel_tools,
            price: self.config.price,
            journal: Arc::clone(&self.journal),
            approvals: Arc::clone(&self.approvals),
            questions: Arc::clone(&self.questions),
            mode: Arc::clone(&self.mode),
            context: self.config.context.clone(),
            memory: Arc::clone(&self.memory),
            cancel: cancel.clone(),
            stop: Arc::clone(&stop),
            pause: Arc::clone(&pause),
            state: Arc::new(std::sync::Mutex::new(initial)),
            finishing: Arc::clone(&finishing),
        };
        let span = tracing::info_span!("task", session = %self.id, task = %task);
        let join =
            tokio::spawn(async move { run_task(ctx, segment, start).await }.instrument(span));
        self.running = Some(Running {
            task,
            cancel,
            stop,
            pause,
            finishing,
            join,
        });
    }

    /// Opens segment 0: gathers the environment and AGENTS.md off the async
    /// workers, stores the frozen frame as a blob and records `SegmentOpened`.
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
        // The new segment's environment states the mode, so notes about
        // earlier switches are not sent (replay drops them the same way).
        let _ = self.mode.take_notes();
        environment
            .notes
            .push(environment_note(self.mode.current(), self.config.approval));
        let segment = SegmentContext::open(SegmentSpec {
            persona: self.config.persona.clone(),
            tools: self.deps.tools.specs(),
            instructions: instructions.render(),
            environment: environment.render(),
            segment: 0,
        });
        let frame = serde_json::to_vec(segment.frame())
            .map_err(|e| format!("cannot encode the segment frame: {e}"))?;
        let frame = self
            .deps
            .blobs
            .put(frame, "application/json", Some("segment-frame.json"))
            .await
            .map_err(|e| format!("cannot store the segment frame: {e}"))?;
        self.journal.set_segment(segment.segment());
        self.journal
            .record(
                None,
                Event::SegmentOpened {
                    segment: segment.segment(),
                    opener: Message::user_text(segment.frame().opener.clone()),
                    frame,
                },
            )
            .await
            .map_err(|e| format!("cannot record the new segment: {e}"))?;
        Ok(segment)
    }

    async fn stop_task(&self, task: TaskId, kind: StopKind) {
        match &self.running {
            Some(running) if running.task == task => {
                *running
                    .stop
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(kind);
                running.cancel.cancel();
            }
            _ => {
                self.warn("no_such_task", format!("task {task} is not running"))
                    .await;
            }
        }
    }

    async fn pause(&self, task: TaskId) {
        match &self.running {
            Some(running) if running.task == task => running.pause.request(),
            _ => {
                self.warn("no_such_task", format!("task {task} is not running"))
                    .await;
            }
        }
    }

    async fn resume(&mut self, task: TaskId) {
        match self.paused.take() {
            Some(paused) if paused.task == task => {
                let Some(segment) = self.segment.take() else {
                    self.error("internal", "the paused task has no segment".into())
                        .await;
                    return;
                };
                self.start_task(task, segment, Start::Continue(paused.round));
            }
            other => {
                self.paused = other;
                self.warn("no_such_task", format!("task {task} is not paused"))
                    .await;
            }
        }
    }

    async fn cancel(&mut self, task: TaskId) {
        if self.running.as_ref().is_some_and(|r| r.task == task) {
            self.stop_task(task, StopKind::Cancel).await;
            return;
        }
        match self.paused.take() {
            Some(paused) if paused.task == task => match self.segment.take() {
                Some(segment) => self.start_task(task, segment, Start::Cancel(paused.round)),
                None => {
                    self.error("internal", "the paused task has no segment".into())
                        .await;
                }
            },
            other => {
                self.paused = other;
                self.warn(
                    "no_such_task",
                    format!("task {task} is not running or paused"),
                )
                .await;
            }
        }
    }

    /// Applies control requests left by other processes.
    async fn poll_controls(&mut self) {
        let controls = match self.deps.sessions.take_controls(self.id).await {
            Ok(controls) => controls,
            Err(e) => {
                tracing::warn!(error = %e, "could not read task control requests");
                return;
            }
        };
        for (task, control) in controls {
            tracing::info!(%task, ?control, "control request from another process");
            match control {
                TaskControl::Pause => self.pause(task).await,
                TaskControl::Resume => self.resume(task).await,
                TaskControl::Cancel => self.cancel(task).await,
            }
        }
    }

    /// Restores the files of the last turn that changed any, records
    /// `UndoApplied`, and queues a note so the model learns what was reverted.
    async fn undo(&mut self, session: SessionId) {
        if session != self.id {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            )
            .await;
            return;
        }
        if self.running.is_some() {
            self.error(
                "busy",
                "a task is running; wait for it or interrupt it before undoing".into(),
            )
            .await;
            return;
        }
        let undone = match self.deps.snapshots.undo_last_turn(self.id).await {
            Ok(Some(undone)) => undone,
            Ok(None) => {
                self.warn("nothing_to_undo", "no file changes left to undo".into())
                    .await;
                return;
            }
            Err(e) => {
                self.error("undo", format!("undo failed: {e}")).await;
                return;
            }
        };
        let task = match self.deps.log.range(self.id, undone.turn, undone.turn).await {
            Ok(events) => events.first().and_then(|e| e.task),
            Err(_) => None,
        };
        let Some(task) = task else {
            self.error(
                "undo",
                format!("event #{} does not belong to a task", undone.turn.0),
            )
            .await;
            return;
        };
        let paths: Vec<String> = undone
            .paths
            .iter()
            .map(|p| p.display().to_string())
            .collect();
        let note = format!(
            "The user reverted these files to their state before your changes in event #{}: {}. Re-read them before editing.",
            undone.turn.0,
            paths.join(", ")
        );
        let _ = self
            .journal
            .record(Some(task), Event::UndoApplied { task, paths })
            .await;
        if self
            .journal
            .record(
                Some(task),
                Event::SystemNote {
                    task,
                    text: note.clone(),
                },
            )
            .await
            .is_ok()
            && let Some(segment) = &mut self.segment
        {
            segment.push_note(note);
        }
    }

    /// Continues the session with `model` at `effort` (ADR-046): compacts into
    /// a new segment that uses them, or, before the first segment, simply
    /// starts with them.
    async fn switch_model(&mut self, session: SessionId, model: ModelRef, effort: Option<Effort>) {
        let (provider, model_id, current_effort) = match &self.segment {
            Some(s) => (
                s.frame().provider.clone(),
                s.frame().model.clone(),
                s.frame().effort,
            ),
            None => (
                self.config.persona.model.provider().to_owned(),
                self.config.persona.model.model().to_owned(),
                self.config.persona.effort,
            ),
        };
        if provider == model.provider() && model_id == model.model() && current_effort == effort {
            self.warn("model", format!("the session already uses {model}"))
                .await;
            return;
        }
        let opened = match self.deps.models.clone() {
            Some(source) => {
                let wanted = model.clone();
                tokio::task::spawn_blocking(move || source.open(&wanted))
                    .await
                    .unwrap_or_else(|e| Err(format!("opening the model failed: {e}")))
            }
            None if provider == model.provider() => {
                Ok((Arc::clone(&self.deps.provider), self.config.price))
            }
            None => Err(format!(
                "this session cannot switch to another provider ({})",
                model.provider()
            )),
        };
        let (next_provider, price) = match opened {
            Ok(opened) => opened,
            Err(message) => {
                self.error("model", message).await;
                return;
            }
        };
        let switch = Switch {
            model,
            effort,
            provider: next_provider,
            price,
        };
        if self
            .segment
            .as_ref()
            .is_some_and(|s| !s.messages().is_empty())
        {
            let change = FrameChange {
                model: Some((
                    switch.model.provider().to_owned(),
                    switch.model.model().to_owned(),
                )),
                effort: Some(effort),
            };
            self.switching = Some(switch);
            self.compact(session, None, change).await;
            if self.running.is_none() {
                // The compaction did not start (busy, wrong session).
                self.switching = None;
            }
        } else if session == self.id {
            // Nothing sent yet: the first segment opens with the new model.
            self.segment = None;
            self.apply_switch(switch).await;
        } else {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            )
            .await;
        }
    }

    /// Makes later model calls use the switched model's provider and price.
    async fn apply_switch(&mut self, switch: Switch) {
        self.deps.provider = switch.provider;
        self.config.price = switch.price;
        self.config.persona.model = switch.model.clone();
        self.config.persona.effort = switch.effort;
        let _ = self
            .journal
            .record(
                None,
                Event::ModelSwitched {
                    model: switch.model,
                    effort: switch.effort,
                },
            )
            .await;
    }

    /// Starts a task that compacts the current segment (`/compact`, or a
    /// model or effort switch).
    async fn compact(&mut self, session: SessionId, focus: Option<String>, change: FrameChange) {
        if session != self.id {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            )
            .await;
            return;
        }
        if self.running.is_some() {
            self.error(
                "busy",
                "a task is running; compact when it has finished".into(),
            )
            .await;
            return;
        }
        if let Some(paused) = &self.paused {
            self.error(
                "busy",
                format!("task {} is paused; resume or cancel it first", paused.task),
            )
            .await;
            return;
        }
        match self.segment.take() {
            Some(segment) if !segment.messages().is_empty() => {
                self.start_task(TaskId::new(), segment, Start::Compact { focus, change });
            }
            other => {
                self.segment = other;
                self.warn("compaction", "nothing to compact yet".into())
                    .await;
            }
        }
    }

    async fn set_mode(&mut self, session: SessionId, mode: Mode) {
        if session != self.id {
            self.error(
                "unknown_session",
                format!("this is session {}, not {session}", self.id),
            )
            .await;
            return;
        }
        if let Err(e) = self.mode.set(mode).await {
            self.warn("mode", e.to_string()).await;
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
            let _ = tokio::time::timeout(Duration::from_secs(5), running.join).await;
        }
        self.root.cancel();
    }

    async fn warn(&self, kind: &str, message: String) {
        let _ = self
            .journal
            .record(
                None,
                Event::Warning {
                    kind: kind.into(),
                    message,
                },
            )
            .await;
    }

    async fn error(&self, kind: &str, message: String) {
        let _ = self
            .journal
            .record(
                None,
                Event::Error {
                    kind: kind.into(),
                    message,
                    hint: None,
                },
            )
            .await;
    }
}

/// The note sent for an attached file (Phase 4): where it is, how big, what
/// kind. The content is not inlined; the model reads it with `read_file`, and
/// Phase 11 adds conversion of images, PDFs and documents.
async fn attachment_note(path: &std::path::Path) -> Result<String, String> {
    let meta = tokio::fs::metadata(path)
        .await
        .map_err(|e| format!("attached file {} not found: {e}", path.display()))?;
    if !meta.is_file() {
        return Err(format!("attached path {} is not a file", path.display()));
    }
    let kind = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp") => "image",
        Some("pdf") => "pdf",
        Some("md" | "markdown") => "text/markdown",
        Some("txt" | "log" | "csv" | "json" | "toml" | "yaml" | "yml" | "xml" | "html") => "text",
        Some(
            "rs" | "py" | "js" | "ts" | "tsx" | "jsx" | "go" | "java" | "c" | "h" | "cpp" | "rb"
            | "sh" | "sql" | "css",
        ) => "source code",
        _ => "file",
    };
    let shown = path.display().to_string().replace('"', "'");
    Ok(format!(
        "<attachment path=\"{shown}\" bytes=\"{}\" type=\"{kind}\">The user attached this file. Read it with read_file if you need its contents.</attachment>",
        meta.len()
    ))
}

/// Waits for the running task, or forever when idle.
async fn join_running(running: &mut Option<Running>) -> Result<TaskResult, tokio::task::JoinError> {
    match running {
        Some(r) => (&mut r.join).await,
        None => std::future::pending().await,
    }
}
