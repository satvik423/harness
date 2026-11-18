//! Phase 3 behaviour of the runtime over the event log: durability, resume,
//! pause, crash recovery, undo and control requests (P3-T3..T7).

#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use harness_context::{SegmentContext, SegmentSpec, builtin_default};
use harness_ports::{
    BlobStore, EventLog, LoggedEvent, Provider, RiskCtx, SearchHit, SessionRecord, SessionStatus,
    SessionStore, StoreError, SystemClock, TaskControl, Tool, ToolCtx, ToolError, ToolName,
    ToolOutput, ToolRegistry, ToolSpec,
};
use harness_protocol::{Decision, Envelope, Event, InputItem, Op, TaskState};
use harness_runtime::{
    ApprovalMode, OUTCOME_UNKNOWN, Resumed, RuntimeDeps, SessionConfig, SessionHandle,
    resume_session, spawn_session,
};
use harness_testkit::invariants::check_all;
use harness_testkit::{AskForChanges, FakeProvider, MemoryStore};
use harness_tools::{MemoryReadLedger, Unsandboxed, WriteFile};
use harness_types::{ContentBlock, Message, Risk, Role, Seq, SessionId, TaskId};
use serde_json::{Value, json};
use tokio::sync::broadcast;

// ---------- test tools ----------

/// Counts runs; read-only, sleeps `ms` (default 0).
struct Slow {
    name: &'static str,
    risk_exec: bool,
    runs: Arc<AtomicUsize>,
}

#[async_trait]
impl Tool for Slow {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new(self.name).unwrap(),
            description: self.name.into(),
            input_schema: json!({"type": "object"}),
            strict: false,
            eager_input_streaming: false,
        }
    }
    fn risk(&self, input: &Value, _: &RiskCtx) -> Risk {
        if self.risk_exec {
            Risk::Exec {
                command: input.to_string(),
            }
        } else {
            Risk::ReadOnly { paths: vec![] }
        }
    }
    async fn run(&self, input: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        self.runs.fetch_add(1, Ordering::SeqCst);
        let ms = input["ms"].as_u64().unwrap_or(0);
        tokio::time::sleep(Duration::from_millis(ms)).await;
        Ok(ToolOutput {
            content: vec![ContentBlock::text(format!(
                "{} done {}",
                self.name,
                input["tag"].as_str().unwrap_or("")
            ))],
            ..ToolOutput::default()
        })
    }
}

struct Env {
    dir: tempfile::TempDir,
    store: Arc<MemoryStore>,
    policy: Arc<AskForChanges>,
    read_runs: Arc<AtomicUsize>,
    exec_runs: Arc<AtomicUsize>,
}

impl Env {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
            store: Arc::new(MemoryStore::default()),
            policy: Arc::new(AskForChanges::default()),
            read_runs: Arc::default(),
            exec_runs: Arc::default(),
        }
    }

    fn deps(&self, provider: Arc<dyn Provider>, log: Arc<dyn EventLog>) -> RuntimeDeps {
        let mut tools = ToolRegistry::new();
        tools
            .register(Arc::new(Slow {
                name: "slow_read",
                risk_exec: false,
                runs: Arc::clone(&self.read_runs),
            }))
            .unwrap();
        tools
            .register(Arc::new(Slow {
                name: "slow_exec",
                risk_exec: true,
                runs: Arc::clone(&self.exec_runs),
            }))
            .unwrap();
        tools.register(Arc::new(WriteFile)).unwrap();
        RuntimeDeps {
            provider,
            tools,
            clock: Arc::new(SystemClock),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::clone(&self.store) as _,
            ledger: Arc::new(MemoryReadLedger::default()),
            policy: Arc::clone(&self.policy) as _,
            log,
            sessions: Arc::clone(&self.store) as _,
            snapshots: Arc::clone(&self.store) as _,
        }
    }

    fn config(&self) -> SessionConfig {
        SessionConfig {
            cwd: self.dir.path().to_path_buf(),
            persona: builtin_default(),
            approval: ApprovalMode::Ask,
            max_turns: 10,
            output_cap_tokens: 2_000,
            max_parallel_tools: 4,
            price: None,
        }
    }

    async fn spawn(&self, fake: &FakeProvider) -> (SessionHandle, broadcast::Receiver<Envelope>) {
        spawn_session(
            self.deps(Arc::new(fake.clone()), Arc::clone(&self.store) as _),
            self.config(),
        )
        .await
        .unwrap()
    }

    async fn resume(
        &self,
        fake: &FakeProvider,
        id: SessionId,
    ) -> (SessionHandle, broadcast::Receiver<Envelope>, Resumed) {
        resume_session(
            self.deps(Arc::new(fake.clone()), Arc::clone(&self.store) as _),
            self.config(),
            id,
        )
        .await
        .unwrap()
    }
}

/// Sends `op` and collects events until a task reaches one of `stop_at`,
/// answering approvals with `decision`. `on_event` may send more ops.
async fn drive(
    handle: &SessionHandle,
    rx: &mut broadcast::Receiver<Envelope>,
    op: Option<Op>,
    stop_at: &[TaskState],
    mut on_event: impl FnMut(&Event) -> Option<Op>,
) -> Vec<Envelope> {
    if let Some(op) = op {
        handle.send(op).await.unwrap();
    }
    let mut seen = Vec::new();
    loop {
        let env = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("event within 10 s")
            .unwrap();
        if let Event::ApprovalRequested { id, .. } = &env.event {
            handle
                .send(Op::Approve {
                    request: *id,
                    decision: Decision::Once,
                })
                .await
                .unwrap();
        }
        if let Some(op) = on_event(&env.event) {
            handle.send(op).await.unwrap();
        }
        let done = matches!(&env.event, Event::TaskState { to, .. } if stop_at.contains(to));
        seen.push(env);
        if done {
            return seen;
        }
    }
}

fn submit(session: SessionId, text: &str) -> Op {
    Op::Submit {
        session,
        input: vec![InputItem::Text { text: text.into() }],
    }
}

const ENDS: &[TaskState] = &[
    TaskState::Completed,
    TaskState::Failed,
    TaskState::Cancelled,
    TaskState::Paused,
    TaskState::BudgetExhausted,
];

fn results_in(req: &harness_ports::ModelRequest) -> Vec<(String, String)> {
    req.messages
        .iter()
        .flat_map(|m| m.content.iter())
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                call_id, content, ..
            } => Some((
                call_id.clone(),
                content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text } => Some(text.clone()),
                        _ => None,
                    })
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

// ---------- durability ----------

#[tokio::test]
async fn every_broadcast_persisted_event_is_already_in_the_log() {
    let env = Env::new();
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "slow_read", &json!({"tag": "a"}))
        .with_text_turn("done");
    let (handle, mut rx) = env.spawn(&fake).await;
    let seen = drive(
        &handle,
        &mut rx,
        Some(submit(handle.id(), "go")),
        ENDS,
        |_| None,
    )
    .await;
    let stored = env.store.events(handle.id());
    for envelope in seen.iter().filter(|e| e.event.is_persisted()) {
        let in_log = stored
            .iter()
            .find(|s| s.envelope.seq == envelope.seq)
            .unwrap_or_else(|| panic!("#{} not stored", envelope.seq.0));
        assert_eq!(in_log.envelope.event, envelope.event);
    }
    // Sequence numbers are gapless from 1.
    let seqs: Vec<u64> = stored.iter().map(|e| e.envelope.seq.0).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    assert!(matches!(
        stored[0].envelope.event,
        Event::SessionOpened { .. }
    ));
    assert!(
        stored
            .iter()
            .any(|e| matches!(e.envelope.event, Event::SegmentOpened { .. }))
    );
}

// ---------- resume ----------

#[tokio::test]
async fn a_resumed_session_continues_the_conversation_with_the_same_prefix() {
    let env = Env::new();
    let first = FakeProvider::new().with_text_turn("Hello!");
    let (handle, mut rx) = env.spawn(&first).await;
    let id = handle.id();
    drive(&handle, &mut rx, Some(submit(id, "hi")), ENDS, |_| None).await;
    handle.send(Op::Shutdown).await.unwrap();
    // The project changes between runs; the segment's prefix must not.
    std::fs::write(env.dir.path().join("AGENTS.md"), "new instructions").unwrap();

    let second = FakeProvider::new().with_text_turn("Again!");
    let (handle, mut rx, resumed) = env.resume(&second, id).await;
    assert_eq!(
        resumed,
        Resumed {
            paused: None,
            recovered: false
        }
    );
    drive(
        &handle,
        &mut rx,
        Some(submit(id, "and again")),
        ENDS,
        |_| None,
    )
    .await;
    let before = &first.requests()[0];
    let after = &second.requests()[0];
    assert_eq!(after.system, before.system);
    assert_eq!(after.tools, before.tools);
    assert_eq!(after.messages.len(), 3);
    assert_eq!(after.messages[..1], before.messages[..]);
    assert_eq!(after.messages[1].text(), "Hello!");
    let mut all = first.requests();
    all.extend(second.requests());
    assert!(check_all(&all).is_empty(), "{:?}", check_all(&all));
}

#[tokio::test]
async fn resuming_an_unknown_session_fails() {
    let env = Env::new();
    let fake = FakeProvider::new();
    let err = resume_session(
        env.deps(Arc::new(fake), Arc::clone(&env.store) as _),
        env.config(),
        SessionId::new(),
    )
    .await
    .err()
    .unwrap();
    assert!(matches!(err, harness_runtime::RuntimeError::NotFound(_)));
}

// ---------- pause ----------

#[tokio::test]
async fn pause_takes_effect_after_the_running_tool_and_resume_after_restart_runs_the_rest() {
    let env = Env::new();
    let calls = [
        ("c1", "slow_read", json!({"ms": 150, "tag": "one"})),
        ("c2", "slow_exec", json!({"tag": "two"})),
    ];
    let first = FakeProvider::new().with_tool_calls_turn(&calls);
    let (handle, mut rx) = env.spawn(&first).await;
    let id = handle.id();
    let mut task = None;
    let seen = drive(
        &handle,
        &mut rx,
        Some(submit(id, "go")),
        ENDS,
        |e| match e {
            Event::UserInput { task: t, .. } => {
                task = Some(*t);
                None
            }
            Event::ToolStarted { call_id } if call_id == "c1" => Some(Op::Pause {
                task: task.unwrap(),
            }),
            _ => None,
        },
    )
    .await;
    assert!(matches!(
        seen.last().unwrap().event,
        Event::TaskState {
            to: TaskState::Paused,
            ..
        }
    ));
    assert_eq!(env.read_runs.load(Ordering::SeqCst), 1, "c1 finished");
    assert_eq!(env.exec_runs.load(Ordering::SeqCst), 0, "c2 did not start");
    handle.send(Op::Shutdown).await.unwrap();

    // A new process: rebuild, then resume.
    let second = FakeProvider::new().with_text_turn("all done");
    let (handle, mut rx, resumed) = env.resume(&second, id).await;
    let task = task.unwrap();
    assert_eq!(
        resumed,
        Resumed {
            paused: Some(task),
            recovered: false
        }
    );
    drive(&handle, &mut rx, Some(Op::Resume { task }), ENDS, |_| None).await;
    assert_eq!(env.read_runs.load(Ordering::SeqCst), 1, "c1 is not re-run");
    assert_eq!(env.exec_runs.load(Ordering::SeqCst), 1, "c2 ran once");
    let req = &second.requests()[0];
    assert_eq!(
        results_in(req),
        [
            ("c1".into(), "slow_read done one".into()),
            ("c2".into(), "slow_exec done two".into())
        ]
    );
    assert!(check_all(&second.requests()).is_empty());
    assert_eq!(
        env.store.tasks(Some(id)).await.unwrap()[0].state,
        TaskState::Completed
    );
}

#[tokio::test]
async fn cancelling_a_paused_task_answers_its_open_calls() {
    let env = Env::new();
    let calls = [
        ("c1", "slow_read", json!({"ms": 100})),
        ("c2", "slow_read_unknown", json!({})),
    ];
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&calls)
        .with_text_turn("next task");
    let (handle, mut rx) = env.spawn(&fake).await;
    let id = handle.id();
    let mut task = None;
    drive(
        &handle,
        &mut rx,
        Some(submit(id, "go")),
        ENDS,
        |e| match e {
            Event::UserInput { task: t, .. } => {
                task = Some(*t);
                None
            }
            Event::ToolStarted { .. } => Some(Op::Pause {
                task: task.unwrap(),
            }),
            _ => None,
        },
    )
    .await;
    let seen = drive(
        &handle,
        &mut rx,
        Some(Op::Cancel {
            task: task.unwrap(),
        }),
        &[TaskState::Cancelled],
        |_| None,
    )
    .await;
    assert!(seen.iter().any(
        |e| matches!(&e.event, Event::ToolFinished { call_id, ok: false, .. } if call_id == "c2")
    ));
    drive(
        &handle,
        &mut rx,
        Some(submit(id, "something else")),
        &[TaskState::Completed],
        |_| None,
    )
    .await;
    assert!(check_all(&fake.requests()).is_empty());
}

#[tokio::test]
async fn a_control_request_in_the_store_pauses_the_running_task() {
    let env = Env::new();
    let calls = [
        ("c1", "slow_read", json!({"ms": 400})),
        ("c2", "slow_exec", json!({})),
    ];
    let fake = FakeProvider::new().with_tool_calls_turn(&calls);
    let (handle, mut rx) = env.spawn(&fake).await;
    let store = Arc::clone(&env.store);
    let mut requested = false;
    let seen = drive(
        &handle,
        &mut rx,
        Some(submit(handle.id(), "go")),
        ENDS,
        |e| {
            if let (Event::ToolStarted { .. }, false) = (e, requested) {
                requested = true;
                let store = Arc::clone(&store);
                let task = store_task(&store, handle.id());
                tokio::spawn(async move { store.request_control(task, TaskControl::Pause).await });
            }
            None
        },
    )
    .await;
    assert!(matches!(
        seen.last().unwrap().event,
        Event::TaskState {
            to: TaskState::Paused,
            ..
        }
    ));
    assert_eq!(env.exec_runs.load(Ordering::SeqCst), 0);
}

fn store_task(store: &MemoryStore, session: SessionId) -> TaskId {
    store.events(session).iter().find_map(|e| e.task).unwrap()
}

// ---------- crash recovery ----------

/// Writes the events a process would have written before dying in the middle
/// of a tool round: c1 (exec) started but never finished, c2 (read) and c3
/// (exec) never started.
async fn crashed_session(env: &Env) -> (SessionId, TaskId) {
    let store = &env.store;
    let id = SessionId::new();
    let task = TaskId::new();
    store
        .create_session(SessionRecord {
            id,
            created_at: 0,
            title: None,
            persona: "default".into(),
            cwd: env.dir.path().into(),
            status: SessionStatus::Open,
            parent: None,
            forked_at: None,
            current_segment: 0,
        })
        .await
        .unwrap();
    let mut segment = SegmentContext::open(SegmentSpec {
        persona: builtin_default(),
        tools: env
            .deps(Arc::new(FakeProvider::new()), store.clone())
            .tools
            .specs(),
        instructions: None,
        environment: "<environment>crash test</environment>".into(),
        segment: 0,
    });
    let frame = store
        .put(
            serde_json::to_vec(segment.frame()).unwrap(),
            "application/json",
            None,
        )
        .await
        .unwrap();
    segment
        .push_user(vec![ContentBlock::text("do things")])
        .unwrap();
    let call = |id: &str, name: &str| ContentBlock::ToolCall {
        id: id.into(),
        name: name.into(),
        input: json!({"tag": id}),
    };
    let assistant = Message::new(
        Role::Assistant,
        vec![
            call("c1", "slow_exec"),
            call("c2", "slow_read"),
            call("c3", "slow_exec"),
        ],
    );
    let events = vec![
        Event::SessionOpened {
            persona: "default".into(),
            segment: 0,
        },
        Event::SegmentOpened {
            segment: 0,
            opener: Message::user_text("<environment>crash test</environment>"),
            frame,
        },
        Event::TaskState {
            task,
            from: TaskState::Queued,
            to: TaskState::Running,
            reason: "started".into(),
        },
        Event::UserInput {
            task,
            message: segment.messages()[0].clone(),
        },
        Event::TurnStarted { task, turn: 1 },
        Event::AssistantMessage {
            task,
            message: assistant,
        },
        Event::ToolCall {
            task,
            id: "c1".into(),
            name: "slow_exec".into(),
            input: json!({"tag": "c1"}),
            risk: Risk::Exec {
                command: "x".into(),
            },
        },
        Event::ToolStarted {
            call_id: "c1".into(),
        },
    ];
    for (n, event) in events.into_iter().enumerate() {
        store
            .append(LoggedEvent {
                envelope: Envelope {
                    seq: Seq(n as u64 + 1),
                    ts: 0,
                    session: id,
                    event,
                },
                task: Some(task),
                segment: 0,
            })
            .await
            .unwrap();
    }
    (id, task)
}

#[tokio::test]
async fn recovery_reports_unfinished_calls_and_resume_reruns_only_unstarted_ones_through_policy() {
    let env = Env::new();
    let (id, task) = crashed_session(&env).await;
    let fake = FakeProvider::new().with_text_turn("recovered fine");
    let (handle, mut rx, resumed) = env.resume(&fake, id).await;
    assert_eq!(
        resumed,
        Resumed {
            paused: Some(task),
            recovered: true
        }
    );

    let log = env.store.events(id);
    let kinds: Vec<&str> = log[8..].iter().map(|e| e.envelope.event.kind()).collect();
    assert_eq!(
        kinds,
        ["task_state", "tool_finished", "task_state", "warning"]
    );
    assert!(matches!(
        &log[9].envelope.event,
        Event::ToolFinished { call_id, ok: false, sent, .. }
            if call_id == "c1" && sent == &vec![ContentBlock::text(OUTCOME_UNKNOWN)]
    ));
    assert_eq!(
        env.store.tasks(Some(id)).await.unwrap()[0].state,
        TaskState::Paused
    );

    // The receiver also saw the recovery events; skip past them.
    loop {
        let env = rx.recv().await.unwrap();
        if matches!(&env.event, Event::Warning { kind, .. } if kind == "recovered") {
            break;
        }
    }
    let seen = drive(&handle, &mut rx, Some(Op::Resume { task }), ENDS, |_| None).await;
    // c1 is never re-run; c2 (read) runs; c3 (exec) is asked about again.
    assert_eq!(env.exec_runs.load(Ordering::SeqCst), 1, "only c3");
    assert_eq!(env.read_runs.load(Ordering::SeqCst), 1, "c2");
    let asked: Vec<&str> = seen
        .iter()
        .filter_map(|e| match &e.event {
            Event::ApprovalRequested { call_id, .. } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(asked, ["c3"]);
    let results = results_in(&fake.requests()[0]);
    assert_eq!(results[0], ("c1".into(), OUTCOME_UNKNOWN.into()));
    assert_eq!(results.len(), 3);
    assert!(check_all(&fake.requests()).is_empty());
}

#[tokio::test]
async fn recovering_twice_is_harmless() {
    let env = Env::new();
    let (id, task) = crashed_session(&env).await;
    let fake = FakeProvider::new();
    let (handle, _rx, _) = env.resume(&fake, id).await;
    handle.send(Op::Shutdown).await.unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    let (_handle, _rx, resumed) = env.resume(&fake, id).await;
    assert_eq!(
        resumed,
        Resumed {
            paused: Some(task),
            recovered: false
        }
    );
}

// ---------- undo ----------

#[tokio::test]
async fn undo_restores_the_last_turns_files_and_tells_the_model() {
    let env = Env::new();
    std::fs::write(env.dir.path().join("keep.txt"), "original").unwrap();
    let fake = FakeProvider::new()
        .with_tool_call_turn(
            "c1",
            "write_file",
            &json!({"path": "new.txt", "content": "created"}),
        )
        .with_text_turn("wrote it")
        .with_text_turn("noted");
    let (handle, mut rx) = env.spawn(&fake).await;
    let id = handle.id();
    drive(&handle, &mut rx, Some(submit(id, "write")), ENDS, |_| None).await;
    assert!(env.dir.path().join("new.txt").exists());

    handle.send(Op::Undo { session: id }).await.unwrap();
    let mut undone = None;
    while undone.is_none() {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::UndoApplied { paths, .. } = env.event {
            undone = Some(paths);
        }
    }
    assert!(
        !env.dir.path().join("new.txt").exists(),
        "the created file is removed"
    );
    assert!(undone.unwrap()[0].ends_with("new.txt"));

    drive(
        &handle,
        &mut rx,
        Some(submit(id, "what happened?")),
        ENDS,
        |_| None,
    )
    .await;
    let last = fake
        .requests()
        .last()
        .unwrap()
        .messages
        .last()
        .unwrap()
        .clone();
    assert!(
        matches!(&last.content[0], ContentBlock::Text { text } if text.starts_with("<system-note>") && text.contains("new.txt")),
        "{last:?}"
    );
    assert!(check_all(&fake.requests()).is_empty());

    // Nothing left: a warning.
    handle.send(Op::Undo { session: id }).await.unwrap();
    loop {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::Warning { kind, .. } = env.event {
            assert_eq!(kind, "nothing_to_undo");
            break;
        }
    }
}

// ---------- log failure ----------

/// An event log that refuses `tool_started` events.
struct RefusesToolStart(Arc<MemoryStore>);

#[async_trait]
impl EventLog for RefusesToolStart {
    async fn append(&self, event: LoggedEvent) -> Result<(), StoreError> {
        if event.envelope.event.kind() == "tool_started" {
            return Err(StoreError::Backend("disk full".into()));
        }
        self.0.append(event).await
    }
    async fn last_seq(&self, session: SessionId) -> Result<Option<Seq>, StoreError> {
        self.0.last_seq(session).await
    }
    async fn range(&self, s: SessionId, f: Seq, t: Seq) -> Result<Vec<LoggedEvent>, StoreError> {
        self.0.range(s, f, t).await
    }
    async fn search(&self, s: SessionId, q: &str, l: usize) -> Result<Vec<SearchHit>, StoreError> {
        self.0.search(s, q, l).await
    }
}

#[tokio::test]
async fn a_tool_never_runs_if_its_start_cannot_be_recorded() {
    let env = Env::new();
    let fake = FakeProvider::new().with_tool_call_turn("c1", "slow_read", &json!({}));
    let log = Arc::new(RefusesToolStart(Arc::clone(&env.store)));
    let (handle, mut rx) = spawn_session(env.deps(Arc::new(fake), log), env.config())
        .await
        .unwrap();
    let seen = drive(
        &handle,
        &mut rx,
        Some(submit(handle.id(), "go")),
        ENDS,
        |_| None,
    )
    .await;
    assert_eq!(env.read_runs.load(Ordering::SeqCst), 0);
    assert!(
        seen.iter()
            .any(|e| matches!(&e.event, Event::Error { kind, .. } if kind == "event_log"))
    );
    assert!(matches!(
        seen.last().unwrap().event,
        Event::TaskState {
            to: TaskState::Failed,
            ..
        }
    ));
}

// Multi-threaded: the actor and the finishing task must really run concurrently.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_prompt_sent_the_moment_a_task_completes_is_not_refused_as_busy() {
    let env = Env::new();
    let mut fake = FakeProvider::new();
    for n in 0..20 {
        fake = fake.with_text_turn(&format!("reply {n}"));
    }
    let (handle, mut rx) = env.spawn(&fake).await;
    let id = handle.id();
    handle.send(submit(id, "0")).await.unwrap();
    let mut completed = 0;
    while completed < 20 {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        match env.event {
            Event::TaskState {
                to: TaskState::Completed,
                ..
            } => {
                completed += 1;
                if completed < 20 {
                    // No pause: this races the actor collecting the finished task.
                    handle
                        .send(submit(id, &completed.to_string()))
                        .await
                        .unwrap();
                }
            }
            Event::Error { kind, message, .. } => panic!("{kind}: {message}"),
            _ => {}
        }
    }
}

#[tokio::test]
async fn a_task_that_stopped_after_its_final_reply_completes_on_resume_without_a_model_call() {
    let env = Env::new();
    let (id, task) = crashed_session(&env).await;
    // Finish the round and record the final reply, but not the Completed state.
    let fake = FakeProvider::new();
    let mut seq = env.store.last_seq(id).await.unwrap().unwrap().0;
    let mut add = |event: Event| {
        seq += 1;
        LoggedEvent {
            envelope: Envelope {
                seq: Seq(seq),
                ts: 0,
                session: id,
                event,
            },
            task: Some(task),
            segment: 0,
        }
    };
    let finished = |call: &str| Event::ToolFinished {
        call_id: call.into(),
        ok: true,
        full: harness_protocol::BlobOrText::Text { text: "ok".into() },
        sent: vec![ContentBlock::text("ok")],
    };
    let events = vec![
        add(finished("c1")),
        add(finished("c2")),
        add(finished("c3")),
        add(Event::TurnStarted { task, turn: 2 }),
        add(Event::AssistantMessage {
            task,
            message: Message::new(Role::Assistant, vec![ContentBlock::text("all done")]),
        }),
    ];
    for e in events {
        env.store.append(e).await.unwrap();
    }
    let (handle, mut rx, resumed) = env.resume(&fake, id).await;
    assert_eq!(resumed.paused, Some(task));
    while rx.try_recv().is_ok() {}
    let seen = drive(&handle, &mut rx, Some(Op::Resume { task }), ENDS, |_| None).await;
    assert!(matches!(
        seen.last().unwrap().event,
        Event::TaskState {
            to: TaskState::Completed,
            ..
        }
    ));
    assert!(fake.requests().is_empty(), "no model call");
}

#[tokio::test]
async fn a_task_whose_input_was_never_saved_is_failed_not_resumed() {
    let env = Env::new();
    let id = SessionId::new();
    let task = TaskId::new();
    env.store
        .create_session(SessionRecord {
            id,
            created_at: 0,
            title: None,
            persona: "default".into(),
            cwd: env.dir.path().into(),
            status: SessionStatus::Open,
            parent: None,
            forked_at: None,
            current_segment: 0,
        })
        .await
        .unwrap();
    let segment = SegmentContext::open(SegmentSpec {
        persona: builtin_default(),
        tools: vec![],
        instructions: None,
        environment: "<environment>x</environment>".into(),
        segment: 0,
    });
    let frame = env
        .store
        .put(
            serde_json::to_vec(segment.frame()).unwrap(),
            "application/json",
            None,
        )
        .await
        .unwrap();
    let events = [
        Event::SessionOpened {
            persona: "default".into(),
            segment: 0,
        },
        Event::SegmentOpened {
            segment: 0,
            opener: Message::user_text("x"),
            frame,
        },
        Event::TaskState {
            task,
            from: TaskState::Queued,
            to: TaskState::Running,
            reason: "started".into(),
        },
    ];
    for (n, event) in events.into_iter().enumerate() {
        env.store
            .append(LoggedEvent {
                envelope: Envelope {
                    seq: Seq(n as u64 + 1),
                    ts: 0,
                    session: id,
                    event,
                },
                task: Some(task),
                segment: 0,
            })
            .await
            .unwrap();
    }
    let fake = FakeProvider::new().with_text_turn("hello");
    let (handle, mut rx, resumed) = env.resume(&fake, id).await;
    assert_eq!(
        resumed,
        Resumed {
            paused: None,
            recovered: false
        }
    );
    let tasks = env.store.tasks(Some(id)).await.unwrap();
    assert_eq!(tasks[0].state, TaskState::Failed);
    assert!(
        tasks[0]
            .reason
            .as_deref()
            .unwrap()
            .contains("submit it again")
    );
    // The session itself is still usable.
    while rx.try_recv().is_ok() {}
    drive(
        &handle,
        &mut rx,
        Some(submit(id, "hi")),
        &[TaskState::Completed],
        |_| None,
    )
    .await;
    assert!(check_all(&fake.requests()).is_empty());
}

// ---------- pricing and attachments (Phase 4) ----------

#[tokio::test]
async fn usage_is_priced_with_the_sessions_model_price() {
    let env = Env::new();
    let fake = FakeProvider::new().with_text_turn("Hello");
    let mut config = env.config();
    config.price = Some(harness_runtime::ModelPrice {
        input: 3.0,
        output: 15.0,
        cache_read: 0.3,
        cache_write: 3.75,
    });
    let (handle, mut rx) = spawn_session(
        env.deps(Arc::new(fake.clone()), Arc::clone(&env.store) as _),
        config,
    )
    .await
    .unwrap();
    let seen = drive(
        &handle,
        &mut rx,
        Some(submit(handle.id(), "hi")),
        ENDS,
        |_| None,
    )
    .await;
    // 10 input tokens × $3/M + 2 output tokens × $15/M = 60 µ$.
    let cost = seen.iter().find_map(|e| match e.event {
        Event::Usage { cost_micros, .. } => Some(cost_micros),
        _ => None,
    });
    assert_eq!(cost, Some(60));
}

#[tokio::test]
async fn attached_files_become_a_reference_note_not_their_content() {
    let env = Env::new();
    std::fs::write(env.dir.path().join("spec.md"), "# secret plan\n").unwrap();
    let fake = FakeProvider::new().with_text_turn("ok");
    let (handle, mut rx) = env.spawn(&fake).await;
    let op = Op::Submit {
        session: handle.id(),
        input: vec![
            InputItem::Text {
                text: "read this".into(),
            },
            InputItem::File {
                path: env.dir.path().join("spec.md"),
            },
        ],
    };
    drive(&handle, &mut rx, Some(op), ENDS, |_| None).await;
    let message = fake.requests()[0].messages[0].clone();
    let texts: Vec<String> = message
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    let note = texts
        .iter()
        .find(|t| t.starts_with("<attachment path="))
        .expect("a note");
    assert!(
        note.contains("spec.md") && note.contains("bytes=\"14\"") && note.contains("read_file"),
        "{note}"
    );
    assert!(
        !texts.iter().any(|t| t.contains("secret plan")),
        "content is not inlined"
    );
    assert!(texts.iter().any(|t| t == "read this"));

    let missing = Op::Submit {
        session: handle.id(),
        input: vec![InputItem::File {
            path: env.dir.path().join("gone.txt"),
        }],
    };
    handle.send(missing).await.unwrap();
    loop {
        let env = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .unwrap()
            .unwrap();
        if let Event::Error { message, .. } = env.event {
            assert!(message.contains("gone.txt"), "{message}");
            break;
        }
    }
}

#[tokio::test]
async fn the_request_history_rebuilds_exactly_what_the_provider_received() {
    let env = Env::new();
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "slow_read", &json!({"tag": "a"}))
        .with_text_turn("done");
    let (handle, mut rx) = env.spawn(&fake).await;
    drive(
        &handle,
        &mut rx,
        Some(submit(handle.id(), "go")),
        ENDS,
        |_| None,
    )
    .await;
    let history =
        harness_runtime::request_history(env.store.as_ref(), env.store.as_ref(), handle.id())
            .await
            .unwrap();
    let rebuilt: Vec<_> = history.into_iter().map(|(_, r)| r).collect();
    assert_eq!(rebuilt, fake.requests());
}
