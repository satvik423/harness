//! Drives one real harness session (runtime, SQLite store, tools) turn by
//! turn, as a user would, and collects what the evals measure.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, bail};
use harness_context::builtin_default;
use harness_context::compaction::CompactionStyle;
use harness_ports::{
    GrantScope, Policy, PolicyCall, PolicyError, Provider, SystemClock, ToolRegistry, Verdict,
};
use harness_protocol::{CompactionRecord, Envelope, Event, InputItem, Op, TaskState};
use harness_runtime::{
    ApprovalMode, ContextSettings, ModelPrice, RuntimeDeps, SessionConfig, SessionHandle,
    spawn_session,
};
use harness_store_sqlite::SqliteStore;
use harness_tools::{MemoryReadLedger, Recall, Unsandboxed};
use harness_types::{Effort, Mode, ModelRef, Seq};
use tokio::sync::broadcast;

/// Longest wait for one turn of a live model.
const TURN_TIMEOUT: Duration = Duration::from_secs(300);

/// Allows every call: the eval sessions only have `recall`, which is read-only.
#[derive(Debug)]
struct AllowAll;

impl Policy for AllowAll {
    fn evaluate(&self, _: &PolicyCall) -> Verdict {
        Verdict::Allow {
            reason: "eval session".into(),
        }
    }

    fn remember(&self, _: &PolicyCall, _: GrantScope) -> Result<String, PolicyError> {
        Ok(String::new())
    }
}

/// How an eval session is set up.
#[derive(Debug, Clone)]
pub(crate) struct Setup {
    /// The model under test.
    pub(crate) model: ModelRef,
    /// Its price, for the cost column.
    pub(crate) price: Option<ModelPrice>,
    /// Effort for the session's model.
    pub(crate) effort: Option<Effort>,
    /// Compact at this many input tokens (small, to force compactions).
    pub(crate) compact_at: u64,
    /// Anchored compaction, or the summary-only baseline (no `recall` either).
    pub(crate) style: CompactionStyle,
}

/// What one turn produced.
#[derive(Debug, Clone, Default)]
pub(crate) struct Turn {
    /// The final reply's text.
    pub(crate) reply: String,
    /// The state the task ended in.
    pub(crate) state: Option<TaskState>,
}

/// A running eval session.
pub(crate) struct Session {
    handle: SessionHandle,
    events: broadcast::Receiver<Envelope>,
    store: Arc<SqliteStore>,
    /// Compactions so far.
    pub(crate) compactions: u32,
    /// Cost so far, in millionths of a dollar.
    pub(crate) cost_micros: u64,
    /// Every compaction record and the opener it produced, in order.
    pub(crate) compacted: Vec<(CompactionRecord, String)>,
}

impl Session {
    /// Opens a session in `dir` (which must outlive it).
    pub(crate) async fn open(
        provider: Arc<dyn Provider>,
        setup: &Setup,
        dir: &Path,
    ) -> anyhow::Result<Self> {
        let store = Arc::new(SqliteStore::open(dir).context("opening the eval store")?);
        let mut tools = ToolRegistry::new();
        if setup.style == CompactionStyle::Anchored {
            tools
                .register(Arc::new(Recall))
                .context("registering recall")?;
        }
        let deps = RuntimeDeps {
            provider,
            tools,
            clock: Arc::new(SystemClock),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::clone(&store) as _,
            ledger: Arc::new(MemoryReadLedger::default()),
            policy: Arc::new(AllowAll),
            log: Arc::clone(&store) as _,
            sessions: Arc::clone(&store) as _,
            snapshots: Arc::clone(&store) as _,
            models: None,
        };
        let mut persona = builtin_default();
        persona.model = setup.model.clone();
        persona.effort = setup.effort;
        let config = SessionConfig {
            cwd: dir.to_path_buf(),
            persona,
            approval: ApprovalMode::Never,
            mode: Mode::Auto,
            full_access: false,
            context: ContextSettings {
                compact_at_tokens: setup.compact_at,
                style: setup.style,
                ..ContextSettings::default()
            },
            max_turns: 8,
            output_cap_tokens: 2_000,
            max_parallel_tools: 4,
            price: setup.price,
        };
        let (handle, events) = spawn_session(deps, config).await?;
        Ok(Self {
            handle,
            events,
            store,
            compactions: 0,
            cost_micros: 0,
            compacted: Vec::new(),
        })
    }

    /// Sends `text` and waits for the task to end.
    pub(crate) async fn ask(&mut self, text: &str) -> anyhow::Result<Turn> {
        self.handle
            .send(Op::Submit {
                session: self.handle.id(),
                input: vec![InputItem::Text { text: text.into() }],
            })
            .await?;
        self.until_task_ends().await
    }

    /// Compacts now and waits for it.
    pub(crate) async fn compact(&mut self) -> anyhow::Result<Turn> {
        self.handle
            .send(Op::Compact {
                session: self.handle.id(),
                focus: None,
            })
            .await?;
        self.until_task_ends().await
    }

    async fn until_task_ends(&mut self) -> anyhow::Result<Turn> {
        let mut turn = Turn::default();
        let mut task = None;
        let mut record: Option<CompactionRecord> = None;
        loop {
            let envelope = match tokio::time::timeout(TURN_TIMEOUT, self.events.recv()).await {
                Err(_) => bail!("no event for {} s", TURN_TIMEOUT.as_secs()),
                Ok(Err(broadcast::error::RecvError::Lagged(_))) => continue,
                Ok(Err(broadcast::error::RecvError::Closed)) => bail!("the session stopped"),
                Ok(Ok(envelope)) => envelope,
            };
            match envelope.event {
                Event::AssistantMessage { message, .. } => {
                    let text = message.text();
                    if !text.trim().is_empty() {
                        turn.reply = text;
                    }
                }
                Event::Usage { cost_micros, .. } => self.cost_micros += cost_micros,
                Event::Compacted { record: r, .. } => {
                    self.compactions += 1;
                    record = Some(*r);
                }
                Event::SegmentOpened { opener, .. } => {
                    if let Some(r) = record.take() {
                        self.compacted.push((r, opener.text()));
                    }
                }
                Event::Error { message, .. } if task.is_none() => bail!("{message}"),
                Event::TaskState { task: t, to, .. }
                    if *task.get_or_insert(t) == t
                        && matches!(
                            to,
                            TaskState::Completed
                                | TaskState::Failed
                                | TaskState::Cancelled
                                | TaskState::BudgetExhausted
                                | TaskState::Paused
                        ) =>
                {
                    turn.state = Some(to);
                    return Ok(turn);
                }
                _ => {}
            }
        }
    }

    /// Every event of the session, for the invariant checks.
    pub(crate) async fn events(&self) -> anyhow::Result<Vec<(Seq, Event)>> {
        use harness_ports::EventLog as _;
        Ok(self
            .store
            .range(self.handle.id(), Seq::FIRST, Seq(u64::MAX))
            .await?
            .into_iter()
            .map(|l| (l.envelope.seq, l.envelope.event))
            .collect())
    }

    /// Stops the session.
    pub(crate) async fn close(self) {
        let _ = self.handle.send(Op::Shutdown).await;
        self.handle.closed().await;
    }
}
