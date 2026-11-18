//! Crash recovery under `kill -9` (P3-T6, NFR-03, FR-SES-04) and pause across a
//! process restart (Phase 3 exit criteria).
//!
//! The test binary re-runs itself as a child process (`crash_child`, selected
//! with an environment variable). The child drives a scripted session against
//! a real SQLite store and prints `ack <seq>` for every event it sees broadcast;
//! the runtime broadcasts only after the commit, so each printed number is a
//! committed event. The parent kills the child with SIGKILL at a random
//! acknowledged event, then recovers the session in-process and checks:
//!
//! - no acknowledged event is lost and numbering is gapless;
//! - recovery and resume produce valid transcripts (I1, I2, no prefill);
//! - no `exec` call ran twice (guardrail 13);
//! - the resumed work completes.
//!
//! `HARNESS_CRASH_RUNS` (default 20) and `HARNESS_CRASH_SEED` control the runs;
//! the Phase 3 exit check uses 200.

#![allow(clippy::unwrap_used, clippy::print_stdout)]

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use harness_context::builtin_default;
use harness_ports::{
    EventLog, GrantScope, Policy, PolicyCall, PolicyError, RiskCtx, SessionStore, SystemClock,
    Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolRegistry, ToolSpec, Verdict,
};
use harness_protocol::{Envelope, Event, InputItem, Op, TaskState};
use harness_runtime::{
    ApprovalMode, RuntimeDeps, SessionConfig, SessionHandle, resume_session, spawn_session,
};
use harness_store_sqlite::SqliteStore;
use harness_testkit::FakeProvider;
use harness_testkit::invariants::check_all;
use harness_tools::{MemoryReadLedger, Unsandboxed};
use harness_types::{Mode, Risk, Seq, SessionId};
use serde_json::{Value, json};
use tokio::sync::broadcast;

const CHILD_DIR: &str = "HARNESS_CRASH_CHILD";
const CHILD_MODE: &str = "HARNESS_CRASH_MODE";

// ---------- tools and policy shared by child and parent ----------

/// Read-only, parallel-safe, a little slow.
struct Probe;
/// Exec: appends its call tag to `bumps.log`, so re-runs are visible.
struct Bump;

fn spec(name: &str) -> ToolSpec {
    ToolSpec {
        name: ToolName::new(name).unwrap(),
        description: name.into(),
        input_schema: json!({"type": "object"}),
        strict: false,
        eager_input_streaming: false,
    }
}

#[async_trait]
impl Tool for Probe {
    fn spec(&self) -> ToolSpec {
        spec("probe")
    }
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: vec![] }
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn run(&self, input: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::time::sleep(Duration::from_millis(2)).await;
        Ok(ToolOutput {
            content: vec![harness_types::ContentBlock::text(format!(
                "probe {}",
                input["tag"]
            ))],
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for Bump {
    fn spec(&self) -> ToolSpec {
        spec("bump")
    }
    fn risk(&self, input: &Value, _: &RiskCtx) -> Risk {
        Risk::Exec {
            command: format!("bump {}", input["tag"]),
        }
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let tag = input["tag"].as_str().unwrap_or("?").to_owned();
        let path = ctx.cwd.join("bumps.log");
        tokio::task::spawn_blocking(move || {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .unwrap();
            writeln!(f, "{tag}").unwrap();
            f.sync_all().unwrap();
        })
        .await
        .unwrap();
        tokio::time::sleep(Duration::from_millis(3)).await;
        Ok(ToolOutput {
            content: vec![harness_types::ContentBlock::text(format!(
                "bumped {}",
                input["tag"]
            ))],
            ..ToolOutput::default()
        })
    }
}

/// Allows everything (approvals are not what this test is about).
struct AllowAll;

impl Policy for AllowAll {
    fn evaluate(&self, _: &PolicyCall) -> Verdict {
        Verdict::Allow {
            reason: "test".into(),
        }
    }
    fn remember(&self, _: &PolicyCall, _: GrantScope) -> Result<String, PolicyError> {
        Ok(String::new())
    }
}

fn deps(store: &Arc<SqliteStore>, fake: &FakeProvider) -> RuntimeDeps {
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(Probe)).unwrap();
    tools.register(Arc::new(Bump)).unwrap();
    tools.register(Arc::new(harness_tools::WriteFile)).unwrap();
    RuntimeDeps {
        provider: Arc::new(fake.clone()),
        tools,
        clock: Arc::new(SystemClock),
        sandbox: Arc::new(Unsandboxed),
        blobs: Arc::clone(store) as _,
        ledger: Arc::new(MemoryReadLedger::default()),
        policy: Arc::new(AllowAll),
        log: Arc::clone(store) as _,
        sessions: Arc::clone(store) as _,
        snapshots: Arc::clone(store) as _,
        models: None,
    }
}

fn config(dir: &Path) -> SessionConfig {
    SessionConfig {
        cwd: dir.to_path_buf(),
        persona: builtin_default(),
        approval: ApprovalMode::Ask,
        mode: Mode::Auto,
        full_access: false,
        context: harness_runtime::ContextSettings::default(),
        max_turns: 50,
        output_cap_tokens: 2_000,
        max_parallel_tools: 4,
        price: None,
    }
}

/// The child's script: rounds of parallel probes, bumps and file writes.
fn long_script() -> FakeProvider {
    let mut fake = FakeProvider::new();
    for round in 0..10 {
        let t = |name: &str| format!("r{round}-{name}");
        fake = fake.with_tool_calls_turn(&[
            (&t("p1"), "probe", json!({"tag": t("p1")})),
            (&t("p2"), "probe", json!({"tag": t("p2")})),
            (&t("b1"), "bump", json!({"tag": t("b1")})),
            (
                &t("w"),
                "write_file",
                json!({"path": format!("out/{round}.txt"), "content": format!("round {round}\n")}),
            ),
            (&t("b2"), "bump", json!({"tag": t("b2")})),
        ]);
    }
    fake.with_text_turn("all rounds done")
}

async fn wait_for(
    rx: &mut broadcast::Receiver<Envelope>,
    stop: impl Fn(&Event) -> bool,
) -> Vec<Envelope> {
    let mut seen: Vec<Envelope> = Vec::new();
    loop {
        let Ok(received) = tokio::time::timeout(Duration::from_secs(20), rx.recv()).await else {
            let kinds: Vec<String> = seen
                .iter()
                .map(|e| match &e.event {
                    Event::Error { kind, message, .. } | Event::Warning { kind, message } => {
                        format!("#{} {kind}: {message}", e.seq.0)
                    }
                    other => format!("#{} {}", e.seq.0, other.kind()),
                })
                .collect();
            panic!("no event within 20 s; saw: {kinds:?}");
        };
        let env = received.unwrap();
        let done = stop(&env.event);
        seen.push(env);
        if done {
            return seen;
        }
    }
}

fn ended(e: &Event) -> bool {
    matches!(
        e,
        Event::TaskState {
            to: TaskState::Completed
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::Paused
                | TaskState::BudgetExhausted,
            ..
        }
    )
}

// ---------- the child ----------

/// Runs only when re-invoked by a parent test.
#[test]
fn crash_child() {
    let Ok(dir) = std::env::var(CHILD_DIR) else {
        return;
    };
    let pause_mode = std::env::var(CHILD_MODE).as_deref() == Ok("pause");
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async move {
        let dir = Path::new(&dir);
        let store = Arc::new(SqliteStore::open(dir).unwrap());
        let fake = long_script();
        let (handle, mut rx) = spawn_session(deps(&store, &fake), config(dir))
            .await
            .unwrap();
        handle
            .send(Op::Submit {
                session: handle.id(),
                input: vec![InputItem::Text {
                    text: "work".into(),
                }],
            })
            .await
            .unwrap();
        let mut out = std::io::stdout();
        let mut task = None;
        let mut started = 0;
        loop {
            let env = rx.recv().await.unwrap();
            if env.event.is_persisted() {
                writeln!(out, "ack {}", env.seq.0).unwrap();
                out.flush().unwrap();
            }
            if let Event::UserInput { task: t, .. } = &env.event {
                task = Some(*t);
            }
            if pause_mode && matches!(env.event, Event::ToolStarted { .. }) {
                started += 1;
                if started == 7 {
                    handle
                        .send(Op::Pause {
                            task: task.unwrap(),
                        })
                        .await
                        .unwrap();
                }
            }
            if ended(&env.event) {
                writeln!(out, "ended").unwrap();
                break;
            }
        }
        handle.send(Op::Shutdown).await.unwrap();
    });
}

// ---------- the parent ----------

/// Small deterministic generator (xorshift64*), so failures can be replayed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
}

/// Spawns the child; kills it after `kill_after` acks (if it gets that far).
/// Returns the highest acknowledged sequence number.
fn run_child(dir: &Path, mode: &str, kill_after: Option<u64>) -> u64 {
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["crash_child", "--exact", "--nocapture", "--test-threads=1"])
        .env(CHILD_DIR, dir)
        .env(CHILD_MODE, mode)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut acked = 0;
    for line in BufReader::new(stdout).lines() {
        let line = line.unwrap();
        if let Some(n) = line.strip_prefix("ack ") {
            acked = n.parse().unwrap();
            if kill_after.is_some_and(|k| acked >= k) {
                // SIGKILL: no destructors, no flushing, like a power cut for the process.
                child.kill().unwrap();
                break;
            }
        }
    }
    let _ = child.wait();
    acked
}

/// Recovers the session, finishes its work and checks every invariant.
async fn recover_and_check(dir: &Path, acked: u64, label: &str) {
    let store = Arc::new(SqliteStore::open(dir).unwrap());
    let sessions = store.sessions().await.unwrap();
    let Some(session) = sessions.first() else {
        assert_eq!(
            acked, 0,
            "{label}: events were acknowledged but no session exists"
        );
        return;
    };
    let id: SessionId = session.id;
    let last = store.last_seq(id).await.unwrap().map_or(0, |s| s.0);
    assert!(
        last >= acked,
        "{label}: acknowledged #{acked} but the log ends at #{last}"
    );
    let all = store.range(id, Seq::FIRST, Seq(u64::MAX)).await.unwrap();
    let seqs: Vec<u64> = all.iter().map(|e| e.envelope.seq.0).collect();
    assert_eq!(
        seqs,
        (1..=last).collect::<Vec<_>>(),
        "{label}: gap in the log"
    );

    let fake = FakeProvider::new()
        .with_text_turn("finished after recovery")
        .with_text_turn("still here");
    let (handle, mut rx, resumed) = resume_session(deps(&store, &fake), config(dir), id)
        .await
        .unwrap_or_else(|e| panic!("{label}: resume failed: {e}"));
    if let Some(task) = resumed.paused {
        while rx.try_recv().is_ok() {}
        handle.send(Op::Resume { task }).await.unwrap();
        let seen = wait_for(&mut rx, ended).await;
        assert!(
            matches!(
                seen.last().unwrap().event,
                Event::TaskState {
                    to: TaskState::Completed,
                    ..
                }
            ),
            "{label}: resumed task did not complete: {:?}",
            seen.last().unwrap().event
        );
    }
    // One more prompt proves the rebuilt transcript is valid for a new request.
    if session_has_segment(&all) {
        send_prompt(&handle, &mut rx).await;
        let violations = check_all(&fake.requests());
        assert!(violations.is_empty(), "{label}: {violations:?}");
    }
    let bumps = std::fs::read_to_string(dir.join("bumps.log")).unwrap_or_default();
    let mut tags: Vec<&str> = bumps.lines().collect();
    let total = tags.len();
    tags.sort_unstable();
    tags.dedup();
    assert_eq!(
        tags.len(),
        total,
        "{label}: an exec call ran twice:\n{bumps}"
    );
    handle.send(Op::Shutdown).await.unwrap();
}

fn session_has_segment(events: &[harness_ports::LoggedEvent]) -> bool {
    events
        .iter()
        .any(|e| matches!(e.envelope.event, Event::SegmentOpened { .. }))
}

async fn send_prompt(handle: &SessionHandle, rx: &mut broadcast::Receiver<Envelope>) {
    while rx.try_recv().is_ok() {}
    handle
        .send(Op::Submit {
            session: handle.id(),
            input: vec![InputItem::Text {
                text: "are you there?".into(),
            }],
        })
        .await
        .unwrap();
    let seen = wait_for(rx, ended).await;
    assert!(matches!(
        seen.last().unwrap().event,
        Event::TaskState {
            to: TaskState::Completed,
            ..
        }
    ));
}

#[test]
fn random_kills_never_lose_acknowledged_events_or_rerun_commands() {
    if std::env::var(CHILD_DIR).is_ok() {
        return;
    }
    let runs: u64 = std::env::var("HARNESS_CRASH_RUNS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(20);
    let seed: u64 = std::env::var("HARNESS_CRASH_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5EED_CAFE);
    // About how many events a complete child run records.
    let full = {
        let dir = tempfile::tempdir().unwrap();
        run_child(dir.path(), "full", None)
    };
    assert!(
        full > 100,
        "the child should record a long session, got {full} events"
    );
    let mut rng = Rng(seed);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    for run in 1..=runs {
        let dir = tempfile::tempdir().unwrap();
        let kill_after = 1 + rng.next() % full;
        let acked = run_child(dir.path(), "kill", Some(kill_after));
        let label =
            format!("run {run}/{runs} (seed {seed}, kill after #{kill_after}, acked #{acked})");
        rt.block_on(recover_and_check(dir.path(), acked, &label));
    }
    println!("{runs} random-kill runs recovered (seed {seed}, {full} events per full run)");
}

#[test]
fn pause_then_process_restart_then_resume_continues_the_task() {
    if std::env::var(CHILD_DIR).is_ok() {
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    run_child(dir.path(), "pause", None);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    rt.block_on(async {
        let store = Arc::new(SqliteStore::open(dir.path()).unwrap());
        let id = store.sessions().await.unwrap()[0].id;
        let tasks = store.tasks(Some(id)).await.unwrap();
        assert_eq!(
            tasks[0].state,
            TaskState::Paused,
            "the child paused at a safe point"
        );
        assert_eq!(tasks[0].reason.as_deref(), Some("paused"));
        let bumps_before = std::fs::read_to_string(dir.path().join("bumps.log")).unwrap();

        // The rest of the script, continued by a new process.
        let fake = continuation_after_pause(&store, id).await;
        let (handle, mut rx, resumed) = resume_session(deps(&store, &fake), config(dir.path()), id)
            .await
            .unwrap();
        assert!(!resumed.recovered);
        let task = resumed.paused.unwrap();
        handle.send(Op::Resume { task }).await.unwrap();
        let seen = wait_for(&mut rx, ended).await;
        assert!(matches!(
            seen.last().unwrap().event,
            Event::TaskState {
                to: TaskState::Completed,
                ..
            }
        ));
        assert!(
            check_all(&fake.requests()).is_empty(),
            "{:?}",
            check_all(&fake.requests())
        );
        let bumps_after = std::fs::read_to_string(dir.path().join("bumps.log")).unwrap();
        assert!(bumps_after.starts_with(&bumps_before), "earlier bumps kept");
        assert!(
            bumps_after.len() > bumps_before.len(),
            "the remaining calls ran"
        );
        let mut tags: Vec<&str> = bumps_after.lines().collect();
        let total = tags.len();
        tags.sort_unstable();
        tags.dedup();
        assert_eq!(tags.len(), total, "no call ran twice");
        handle.send(Op::Shutdown).await.unwrap();
    });
}

/// A provider that continues the long script from where the paused session
/// stopped: the turns it already consumed are skipped.
async fn continuation_after_pause(store: &Arc<SqliteStore>, id: SessionId) -> FakeProvider {
    let events = store.range(id, Seq::FIRST, Seq(u64::MAX)).await.unwrap();
    let replies = events
        .iter()
        .filter(|e| matches!(e.envelope.event, Event::AssistantMessage { .. }))
        .count();
    let mut fake = long_script();
    for _ in 0..replies {
        fake.skip_turn();
    }
    fake
}
