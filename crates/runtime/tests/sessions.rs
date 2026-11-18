//! Scripted end-to-end sessions against the runtime (P1-T6, P1-T9 exit criterion).
//!
//! Each session drives the real session actor with a [`FakeProvider`] and test
//! tools, then checks the transcript invariants (append-only I1, tool pairing
//! I2, no prefill) over every request the provider received.

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::stream;
use harness_context::builtin_default;
use harness_ports::{
    Capabilities, ModelRequest, Provider, ProviderError, ProviderEvent, ProviderStream, RiskCtx,
    SystemClock, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolRegistry, ToolSpec,
};
use harness_protocol::{Decision, Envelope, Event, InputItem, Op, TaskState};
use harness_runtime::{ApprovalMode, RuntimeDeps, SessionConfig, SessionHandle, spawn_session};
use harness_testkit::invariants::check_all;
use harness_testkit::{AskForChanges, FakeProvider, MemoryStore};
use harness_tools::{MemoryReadLedger, Unsandboxed};
use harness_types::{ContentBlock, Risk, Role, StopReason, TaskId};
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

// ---------- test tools ----------

/// Read-only tool that echoes its `text` input.
struct Echo;
/// Tool with Exec risk (needs approval) that reports it ran.
struct Exec;
/// Read-only tool that always fails.
struct Fails;
/// Like [`Echo`], but with its own 5-token output cap.
struct EchoTinyCap;
/// Parallel-safe read that sleeps, tracking how many run at once.
struct SlowRead {
    running: Arc<std::sync::atomic::AtomicUsize>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
}

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
impl Tool for Echo {
    fn spec(&self) -> ToolSpec {
        spec("echo")
    }
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: vec![] }
    }
    async fn run(&self, input: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ContentBlock::text(input["text"].as_str().unwrap_or(""))],
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for Exec {
    fn spec(&self) -> ToolSpec {
        spec("exec")
    }
    fn risk(&self, input: &Value, _: &RiskCtx) -> Risk {
        Risk::Exec {
            command: input["command"].as_str().unwrap_or("").into(),
        }
    }
    async fn run(&self, _: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput {
            content: vec![ContentBlock::text("ran")],
            ..ToolOutput::default()
        })
    }
}

#[async_trait]
impl Tool for Fails {
    fn spec(&self) -> ToolSpec {
        spec("fails")
    }
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: vec![] }
    }
    async fn run(&self, _: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        Err(ToolError::Failed("disk on fire".into()))
    }
}

#[async_trait]
impl Tool for EchoTinyCap {
    fn spec(&self) -> ToolSpec {
        spec("echo_tiny_cap")
    }
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: vec![] }
    }
    fn output_cap_tokens(&self) -> Option<u64> {
        Some(5)
    }
    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Echo.run(input, ctx).await
    }
}

#[async_trait]
impl Tool for SlowRead {
    fn spec(&self) -> ToolSpec {
        spec("slow_read")
    }
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: vec![] }
    }
    fn parallel_safe(&self) -> bool {
        true
    }
    async fn run(&self, input: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
        use std::sync::atomic::Ordering;
        let now = self.running.fetch_add(1, Ordering::SeqCst) + 1;
        self.peak.fetch_max(now, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(150)).await;
        self.running.fetch_sub(1, Ordering::SeqCst);
        Ok(ToolOutput {
            content: vec![ContentBlock::text(input["text"].as_str().unwrap_or(""))],
            ..ToolOutput::default()
        })
    }
}

/// A provider whose stream never yields until cancelled (for interrupt tests).
struct Hanging;

#[async_trait]
impl Provider for Hanging {
    fn id(&self) -> &str {
        "hanging"
    }
    async fn capabilities(&self, _: &str) -> Result<Capabilities, ProviderError> {
        Err(ProviderError::Unsupported("test".into()))
    }
    async fn stream(
        &self,
        _: ModelRequest,
        _cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError> {
        Ok(Box::pin(
            stream::iter(vec![Ok(ProviderEvent::TextDelta {
                text: "partial".into(),
            })])
            .chain(stream::pending()),
        ))
    }
    async fn count_tokens(&self, _: &ModelRequest) -> Result<u64, ProviderError> {
        Ok(0)
    }
}

use futures::StreamExt;

// ---------- harness ----------

struct Run {
    handle: SessionHandle,
    events: broadcast::Receiver<Envelope>,
    blobs: Arc<MemoryStore>,
    policy: Arc<AskForChanges>,
    peak: Arc<std::sync::atomic::AtomicUsize>,
    _dir: tempfile::TempDir,
}

/// Output cap used by the test sessions (small, so tests can exceed it cheaply).
const TEST_CAP_TOKENS: u64 = 200;

async fn start(provider: Arc<dyn Provider>, approval: ApprovalMode, max_turns: u32) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(Echo)).unwrap();
    tools.register(Arc::new(Exec)).unwrap();
    tools.register(Arc::new(Fails)).unwrap();
    tools.register(Arc::new(EchoTinyCap)).unwrap();
    tools.register(Arc::new(harness_tools::AskUser)).unwrap();
    let peak = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    tools
        .register(Arc::new(SlowRead {
            running: Arc::default(),
            peak: Arc::clone(&peak),
        }))
        .unwrap();
    let store = Arc::new(MemoryStore::default());
    let policy = Arc::new(AskForChanges::default());
    let deps = RuntimeDeps {
        provider,
        tools,
        clock: Arc::new(SystemClock),
        sandbox: Arc::new(Unsandboxed),
        blobs: Arc::clone(&store) as _,
        ledger: Arc::new(MemoryReadLedger::default()),
        policy: Arc::clone(&policy) as _,
        log: Arc::clone(&store) as _,
        sessions: Arc::clone(&store) as _,
        snapshots: Arc::clone(&store) as _,
    };
    let config = SessionConfig {
        cwd: dir.path().to_path_buf(),
        persona: builtin_default(),
        approval,
        max_turns,
        output_cap_tokens: TEST_CAP_TOKENS,
        max_parallel_tools: 2,
    };
    let (handle, events) = spawn_session(deps, config).await.unwrap();
    Run {
        handle,
        events,
        blobs: store,
        policy,
        peak,
        _dir: dir,
    }
}

/// What to do when an approval is requested.
#[derive(Clone, Copy)]
enum OnApproval {
    Approve,
    Deny,
    Interrupt,
    ForSession,
    AlwaysUser,
}

fn ends_task(e: &Event) -> bool {
    matches!(
        e,
        Event::TaskState {
            to: TaskState::Completed
                | TaskState::Failed
                | TaskState::Cancelled
                | TaskState::BudgetExhausted
                | TaskState::Paused,
            ..
        }
    )
}

/// Submits `text` and collects events until the task ends.
async fn submit(run: &mut Run, text: &str, on_approval: OnApproval) -> Vec<Event> {
    submit_envelopes(run, text, on_approval)
        .await
        .into_iter()
        .map(|e| e.event)
        .collect()
}

/// Like [`submit`], but keeps the envelopes (sequence numbers).
async fn submit_envelopes(run: &mut Run, text: &str, on_approval: OnApproval) -> Vec<Envelope> {
    run.handle
        .send(Op::Submit {
            session: run.handle.id(),
            input: vec![InputItem::Text { text: text.into() }],
        })
        .await
        .unwrap();
    let mut events = Vec::new();
    let mut task: Option<TaskId> = None;
    loop {
        let env = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .expect("event within 10 s")
            .unwrap();
        let event = &env.event;
        if let Event::UserInput { task: t, .. } = event {
            task = Some(*t);
        }
        if let Event::QuestionAsked { id, .. } = event {
            let op = match on_approval {
                OnApproval::Interrupt => Op::Interrupt {
                    task: task.unwrap(),
                },
                OnApproval::Approve
                | OnApproval::Deny
                | OnApproval::ForSession
                | OnApproval::AlwaysUser => Op::Answer {
                    request: *id,
                    answer: "blue".into(),
                },
            };
            run.handle.send(op).await.unwrap();
        }
        if let Event::ApprovalRequested { id, .. } = event {
            let op = match on_approval {
                OnApproval::Approve => Op::Approve {
                    request: *id,
                    decision: Decision::Once,
                },
                OnApproval::Deny => Op::Approve {
                    request: *id,
                    decision: Decision::Deny {
                        reason: Some("not today".into()),
                    },
                },
                OnApproval::Interrupt => Op::Interrupt {
                    task: task.unwrap(),
                },
                OnApproval::ForSession => Op::Approve {
                    request: *id,
                    decision: Decision::ForSession,
                },
                OnApproval::AlwaysUser => Op::Approve {
                    request: *id,
                    decision: Decision::Always {
                        scope: harness_protocol::RuleScope::User,
                    },
                },
            };
            run.handle.send(op).await.unwrap();
        }
        // Only the task this submission started counts (a superseded paused
        // task ends first).
        let done = ends_task(event)
            && matches!(event, Event::TaskState { task: t, .. } if Some(*t) == task);
        events.push(env);
        if done {
            return events;
        }
    }
}

fn final_state(events: &[Event]) -> TaskState {
    match events.last() {
        Some(Event::TaskState { to, .. }) => *to,
        other => panic!("last event is not a task state: {other:?}"),
    }
}

fn assert_invariants(fake: &FakeProvider) {
    let violations = check_all(&fake.requests());
    assert!(
        violations.is_empty(),
        "invariant violations: {violations:?}"
    );
}

fn tool_results(req: &ModelRequest) -> Vec<(String, bool, String)> {
    req.messages
        .last()
        .unwrap()
        .content
        .iter()
        .filter_map(|b| match b {
            ContentBlock::ToolResult {
                call_id,
                content,
                is_error,
            } => Some((
                call_id.clone(),
                *is_error,
                content
                    .iter()
                    .filter_map(|c| match c {
                        ContentBlock::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect(),
            )),
            _ => None,
        })
        .collect()
}

// ---------- the five scripted sessions ----------

#[tokio::test]
async fn session_1_text_only_reply_completes() {
    let fake = FakeProvider::new().with_text_turn("Hello!");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "hi", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    assert!(events.iter().any(
        |e| matches!(e, Event::AssistantMessage { message, .. } if message.text() == "Hello!")
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::TextDelta { text, .. } if text == "Hello!"))
    );
    let req = &fake.requests()[0];
    assert_eq!(req.messages.len(), 1);
    assert!(
        matches!(&req.messages[0].content[0], ContentBlock::Text { text } if text.starts_with("<environment>"))
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn session_2_read_only_tool_runs_without_approval() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo", &json!({"text": "pong"}))
        .with_text_turn("It said pong.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "ping", OnApproval::Deny).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequested { .. }))
    );
    assert_eq!(
        tool_results(&fake.requests()[1]),
        vec![("c1".into(), false, "pong".into())]
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn session_3_two_calls_in_one_turn_return_results_in_one_message() {
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&[
            ("a", "echo", json!({"text": "one"})),
            ("b", "exec", json!({"command": "make"})),
        ])
        .with_text_turn("Both done.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "do both", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    let approvals = events
        .iter()
        .filter(|e| matches!(e, Event::ApprovalRequested { .. }))
        .count();
    assert_eq!(approvals, 1, "only the exec call needs approval");
    assert_eq!(
        tool_results(&fake.requests()[1]),
        vec![
            ("a".into(), false, "one".into()),
            ("b".into(), false, "ran".into())
        ]
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn session_4_conversation_continues_across_tasks() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo", &json!({"text": "x"}))
        .with_text_turn("First answer.")
        .with_tool_call_turn("c2", "exec", &json!({"command": "ls"}))
        .with_text_turn("Second answer.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    assert_eq!(
        final_state(&submit(&mut run, "first", OnApproval::Approve).await),
        TaskState::Completed
    );
    assert_eq!(
        final_state(&submit(&mut run, "second", OnApproval::Approve).await),
        TaskState::Completed
    );
    let requests = fake.requests();
    assert_eq!(requests.len(), 4);
    let last = requests.last().unwrap();
    let roles: Vec<Role> = last.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant,
            Role::User,
            Role::Assistant,
            Role::User
        ]
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn session_5_denied_unknown_and_failing_tools_are_reported_to_the_model() {
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&[
            ("a", "exec", json!({"command": "rm -rf /"})),
            ("b", "nope", json!({})),
            ("c", "fails", json!({})),
        ])
        .with_text_turn("I'll stop.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Deny).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    let results = tool_results(&fake.requests()[1]);
    assert_eq!(results.len(), 3);
    assert!(
        results.iter().all(|(_, is_error, _)| *is_error),
        "{results:?}"
    );
    assert!(
        results[0].2.contains("denied") && results[0].2.contains("not today"),
        "{results:?}"
    );
    assert!(results[1].2.contains("unknown tool"), "{results:?}");
    assert!(results[2].2.contains("disk on fire"), "{results:?}");
    assert_invariants(&fake);
}

// ---------- further behaviour ----------

#[tokio::test]
async fn approval_never_denies_without_asking() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "exec", &json!({"command": "ls"}))
        .with_text_turn("ok");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Never, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequested { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Warning { kind, .. } if kind == "approval_denied"))
    );
    assert!(tool_results(&fake.requests()[1])[0].1);
    assert_invariants(&fake);
}

#[tokio::test]
async fn interrupt_during_approval_pauses_with_a_valid_transcript() {
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&[
            ("a", "exec", json!({"command": "x"})),
            ("b", "echo", json!({"text": "y"})),
        ])
        .with_text_turn("resumed");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Interrupt).await;
    assert_eq!(final_state(&events), TaskState::Paused);
    // The next input still works, and its request carries error results for both calls.
    assert_eq!(
        final_state(&submit(&mut run, "continue", OnApproval::Approve).await),
        TaskState::Completed
    );
    let results = tool_results_before_last_user(&fake.requests()[1]);
    assert_eq!(results.len(), 2);
    assert!(
        results
            .iter()
            .all(|(_, is_error, text)| *is_error && text.contains("interrupted")),
        "{results:?}"
    );
    assert_invariants(&fake);
}

fn tool_results_before_last_user(req: &ModelRequest) -> Vec<(String, bool, String)> {
    let mut trimmed = req.clone();
    trimmed.messages.pop();
    tool_results(&trimmed)
}

#[tokio::test]
async fn interrupt_during_streaming_records_the_partial_text_and_pauses() {
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10).await;
    run.handle
        .send(Op::Submit {
            session: run.handle.id(),
            input: vec![InputItem::Text { text: "hi".into() }],
        })
        .await
        .unwrap();
    let mut events = Vec::new();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap()
            .event;
        if let Event::TextDelta { task, .. } = &event {
            run.handle
                .send(Op::Interrupt { task: *task })
                .await
                .unwrap();
        }
        let done = ends_task(&event);
        events.push(event);
        if done {
            break;
        }
    }
    assert_eq!(final_state(&events), TaskState::Paused);
    assert!(events.iter().any(
        |e| matches!(e, Event::AssistantAborted { partial_text, .. } if partial_text == "partial")
    ));
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::AssistantMessage { .. }))
    );
}

#[tokio::test]
async fn max_turns_exhausts_the_budget() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("a", "echo", &json!({"text": "1"}))
        .with_tool_call_turn("b", "echo", &json!({"text": "2"}))
        .with_text_turn("never reached");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 2).await;
    let events = submit(&mut run, "loop", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::BudgetExhausted);
    assert_eq!(fake.requests().len(), 2);
    assert_invariants(&fake);
}

#[tokio::test]
async fn provider_error_fails_the_task_with_an_error_event() {
    let fake = FakeProvider::new().with_turn(vec![Err(ProviderError::Auth("bad key".into()))]);
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "hi", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Failed);
    assert!(
        events.iter().any(
            |e| matches!(e, Event::Error { kind, hint: Some(_), .. } if kind == "provider_auth")
        )
    );
}

#[tokio::test]
async fn refusal_fails_the_task_and_the_next_input_still_works() {
    let fake = FakeProvider::new()
        .with_turn(vec![
            Ok(ProviderEvent::Usage {
                usage: harness_types::Usage::default(),
            }),
            Ok(ProviderEvent::Stop {
                reason: StopReason::Refusal {
                    category: Some("cyber".into()),
                },
            }),
        ])
        .with_text_turn("Sure, here is something else.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "bad request", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Failed);
    assert!(events.iter().any(|e| matches!(
        e,
        Event::TurnFinished {
            stop: StopReason::Refusal { .. },
            ..
        }
    )));
    assert_eq!(
        final_state(&submit(&mut run, "something else", OnApproval::Approve).await),
        TaskState::Completed
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn submitting_while_busy_is_an_error() {
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10).await;
    let submit_op = || Op::Submit {
        session: run.handle.id(),
        input: vec![InputItem::Text { text: "hi".into() }],
    };
    run.handle.send(submit_op()).await.unwrap();
    run.handle.send(submit_op()).await.unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap()
            .event;
        if let Event::Error { kind, .. } = &event {
            assert_eq!(kind, "busy");
            break;
        }
    }
}

#[tokio::test]
async fn session_opened_is_the_first_event() {
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10).await;
    let first = run.events.recv().await.unwrap();
    assert!(
        matches!(first.event, Event::SessionOpened { segment: 0, .. }),
        "{first:?}"
    );
    assert_eq!(first.seq, harness_types::Seq::FIRST);
}

// ---------- edge cases and less common paths ----------

/// Sends `op` and returns the first warning or error that follows.
async fn first_problem(run: &mut Run, op: Op) -> Event {
    run.handle.send(op).await.unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap()
            .event;
        if matches!(event, Event::Warning { .. } | Event::Error { .. }) {
            return event;
        }
    }
}

fn kind_of(e: &Event) -> &str {
    match e {
        Event::Warning { kind, .. } | Event::Error { kind, .. } => kind,
        _ => "",
    }
}

#[tokio::test]
async fn cancel_during_approval_ends_cancelled() {
    let fake = FakeProvider::new().with_tool_call_turn("c1", "exec", &json!({"command": "x"}));
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    run.handle
        .send(Op::Submit {
            session: run.handle.id(),
            input: vec![InputItem::Text { text: "go".into() }],
        })
        .await
        .unwrap();
    let mut task = None;
    let state = loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap()
            .event;
        if let Event::UserInput { task: t, .. } = &event {
            task = Some(*t);
        }
        if matches!(event, Event::ApprovalRequested { .. }) {
            run.handle
                .send(Op::Cancel {
                    task: task.unwrap(),
                })
                .await
                .unwrap();
        }
        if ends_task(&event) {
            break final_state(&[event]);
        }
    };
    assert_eq!(state, TaskState::Cancelled);
}

#[tokio::test]
async fn ops_from_later_phases_are_reported_as_unsupported() {
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10).await;
    let session = run.handle.id();
    for op in [
        Op::Pause {
            task: TaskId::new(),
        },
        Op::Resume {
            task: TaskId::new(),
        },
    ] {
        assert_eq!(kind_of(&first_problem(&mut run, op).await), "no_such_task");
    }
    for op in [
        Op::Compact {
            session,
            focus: None,
        },
        Op::SwitchPersona {
            session,
            persona: "x".into(),
        },
        Op::Fork {
            session,
            at: harness_types::Seq(1),
        },
        Op::ResumeSession { id: session },
    ] {
        assert_eq!(kind_of(&first_problem(&mut run, op).await), "unsupported");
    }
}

#[tokio::test]
async fn bad_submissions_are_rejected_with_specific_errors() {
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10).await;
    let session = run.handle.id();
    let wrong = Op::Submit {
        session: harness_types::SessionId::new(),
        input: vec![InputItem::Text { text: "x".into() }],
    };
    assert_eq!(
        kind_of(&first_problem(&mut run, wrong).await),
        "unknown_session"
    );
    let file = Op::Submit {
        session,
        input: vec![InputItem::File {
            path: "a.pdf".into(),
        }],
    };
    assert_eq!(kind_of(&first_problem(&mut run, file).await), "unsupported");
    let empty = Op::Submit {
        session,
        input: vec![],
    };
    assert_eq!(
        kind_of(&first_problem(&mut run, empty).await),
        "empty_input"
    );
}

#[tokio::test]
async fn stray_approvals_and_interrupts_are_warnings() {
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10).await;
    let approve = Op::Approve {
        request: harness_types::ApprovalId::new(),
        decision: Decision::Once,
    };
    assert_eq!(
        kind_of(&first_problem(&mut run, approve).await),
        "unknown_approval"
    );
    assert_eq!(
        kind_of(
            &first_problem(
                &mut run,
                Op::Interrupt {
                    task: TaskId::new()
                }
            )
            .await
        ),
        "no_such_task"
    );
}

#[tokio::test]
async fn pause_turn_ends_the_task_with_a_warning_and_stays_append_only() {
    let fake = FakeProvider::new()
        .with_turn(vec![
            Ok(ProviderEvent::BlockComplete {
                block: ContentBlock::text("Working…"),
            }),
            Ok(ProviderEvent::Stop {
                reason: StopReason::PauseTurn,
            }),
        ])
        .with_text_turn("Next answer.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Warning { kind, .. } if kind == "pause_turn"))
    );
    assert_eq!(
        fake.requests().len(),
        1,
        "no assistant-final request is sent"
    );
    assert_eq!(
        final_state(&submit(&mut run, "next", OnApproval::Approve).await),
        TaskState::Completed
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn max_tokens_without_tool_calls_fails() {
    let fake = FakeProvider::new().with_turn(vec![
        Ok(ProviderEvent::BlockComplete {
            block: ContentBlock::text("This answer is cut"),
        }),
        Ok(ProviderEvent::Stop {
            reason: StopReason::MaxTokens,
        }),
    ]);
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Failed);
}

#[tokio::test]
async fn provider_notices_become_warnings() {
    let fake = FakeProvider::new().with_turn(vec![
        Ok(ProviderEvent::Notice {
            message: "thinking dropped".into(),
        }),
        Ok(ProviderEvent::BlockComplete {
            block: ContentBlock::text("ok"),
        }),
        Ok(ProviderEvent::Stop {
            reason: StopReason::EndTurn,
        }),
    ]);
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert!(events.iter().any(|e| matches!(e, Event::Warning { kind, message } if kind == "provider_notice" && message == "thinking dropped")));
}

#[tokio::test]
async fn shutdown_cancels_a_running_task() {
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10).await;
    run.handle
        .send(Op::Submit {
            session: run.handle.id(),
            input: vec![InputItem::Text { text: "hi".into() }],
        })
        .await
        .unwrap();
    loop {
        let event = tokio::time::timeout(Duration::from_secs(10), run.events.recv())
            .await
            .unwrap()
            .unwrap()
            .event;
        if matches!(event, Event::TextDelta { .. }) {
            run.handle.send(Op::Shutdown).await.unwrap();
        }
        if let Event::TaskState {
            to: TaskState::Cancelled,
            ..
        } = event
        {
            break;
        }
    }
}

// ---------- output capping (P2-T3) ----------

fn numbered_lines(n: usize) -> String {
    (1..=n)
        .map(|i| format!("line {i:05}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn large_output_is_capped_once_with_a_footer_naming_the_event_that_holds_it() {
    let big = numbered_lines(8_000); // ~88 KB, over the 64 KB blob threshold
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo", &json!({ "text": big }))
        .with_text_turn("done");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let envelopes = submit_envelopes(&mut run, "go", OnApproval::Deny).await;

    let (seq, full, sent) = envelopes
        .iter()
        .find_map(|e| match &e.event {
            Event::ToolFinished { full, sent, .. } => Some((e.seq, full.clone(), sent.clone())),
            _ => None,
        })
        .unwrap();
    let results = tool_results(&fake.requests()[1]);
    let text = &results[0].2;
    assert!(text.starts_with("line 00001\n"), "{text}");
    assert!(
        text.ends_with(&format!(
            "full: recall {{\"seq_from\":{0},\"seq_to\":{0}}}]",
            seq.0
        )),
        "{text}"
    );
    assert!(harness_context::estimate_tokens(text) < TEST_CAP_TOKENS + 40);
    // The event's `sent` is exactly what the model received.
    assert_eq!(sent, vec![ContentBlock::text(text.clone())]);
    // Over 64 KB, the full output is kept as a blob.
    let harness_protocol::BlobOrText::Blob { blob, .. } = full else {
        panic!("expected a blob, got {full:?}");
    };
    use harness_ports::BlobStore;
    let bytes = run.blobs.get(&blob).await.unwrap();
    assert_eq!(String::from_utf8(bytes).unwrap(), big);
    assert_invariants(&fake);
}

#[tokio::test]
async fn small_output_is_sent_whole_and_kept_inline() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo", &json!({"text": "short"}))
        .with_text_turn("done");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Deny).await;
    assert!(events.iter().any(|e| matches!(
        e,
        Event::ToolFinished { full: harness_protocol::BlobOrText::Text { text }, .. } if text == "short"
    )));
    assert_eq!(tool_results(&fake.requests()[1])[0].2, "short");
}

#[tokio::test]
async fn a_tools_own_cap_overrides_the_session_cap() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo_tiny_cap", &json!({"text": numbered_lines(20)}))
        .with_text_turn("done");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    submit(&mut run, "go", OnApproval::Deny).await;
    let text = &tool_results(&fake.requests()[1])[0].2;
    assert!(text.contains("[output trimmed: "), "{text}");
}

// ---------- ask_user (P2-T8) ----------

#[tokio::test]
async fn ask_user_waits_for_the_answer_and_returns_it() {
    let fake = FakeProvider::new()
        .with_tool_call_turn(
            "q1",
            "ask_user",
            &json!({"question": "Favourite colour?", "options": ["red", "blue"]}),
        )
        .with_text_turn("Blue it is.");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "pick", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    let asked = events
        .iter()
        .position(|e| {
            matches!(e, Event::QuestionAsked { question, options, call_id, .. }
            if question == "Favourite colour?" && options == &["red", "blue"] && call_id == "q1")
        })
        .expect("question asked");
    assert!(matches!(
        &events[asked - 1],
        Event::TaskState {
            to: TaskState::AwaitingApproval,
            ..
        }
    ));
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::QuestionAnswered { answer, .. } if answer == "blue"))
    );
    assert_eq!(
        tool_results(&fake.requests()[1]),
        vec![("q1".into(), false, "The user answered: blue".into())]
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn ask_user_in_a_headless_session_answers_no_user_available() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("q1", "ask_user", &json!({"question": "?"}))
        .with_text_turn("ok");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Never, 10).await;
    let events = submit(&mut run, "pick", OnApproval::Approve).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::QuestionAsked { .. }))
    );
    assert_eq!(
        tool_results(&fake.requests()[1])[0].2,
        harness_tools::NO_USER_ANSWER
    );
}

#[tokio::test]
async fn interrupting_a_question_pauses_with_a_valid_transcript() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("q1", "ask_user", &json!({"question": "?"}))
        .with_text_turn("after resume");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "pick", OnApproval::Interrupt).await;
    assert_eq!(final_state(&events), TaskState::Paused);
    // The next task still gets a well-formed transcript.
    submit(&mut run, "continue", OnApproval::Approve).await;
    let req = fake.requests().last().unwrap().clone();
    assert_eq!(tool_results_before_last_user(&req)[0].0, "q1");
    assert_invariants(&fake);
}

#[tokio::test]
async fn stray_answers_are_warnings() {
    let fake = FakeProvider::new();
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10).await;
    let e = first_problem(
        &mut run,
        Op::Answer {
            request: harness_types::QuestionId::new(),
            answer: "x".into(),
        },
    )
    .await;
    assert!(matches!(e, Event::Warning { kind, .. } if kind == "unknown_question"));
}

// ---------- policy and approvals (P2-T6) ----------

#[tokio::test]
async fn policy_denials_reach_the_model_without_asking() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "echo", &json!({"text": "x", "deny": true}))
        .with_text_turn("ok");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert!(
        !events
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequested { .. }))
    );
    assert!(
        events
            .iter()
            .any(|e| matches!(e, Event::Warning { kind, .. } if kind == "policy_denied"))
    );
    let results = tool_results(&fake.requests()[1]);
    assert!(results[0].1);
    assert!(results[0].2.starts_with("Denied by policy"), "{results:?}");
    assert_invariants(&fake);
}

#[tokio::test]
async fn approval_requests_carry_the_policys_reason() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "exec", &json!({"command": "ls"}))
        .with_text_turn("ok");
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert!(events.iter().any(
        |e| matches!(e, Event::ApprovalRequested { reason, .. } if reason == "exec needs approval")
    ));
}

#[tokio::test]
async fn approving_for_the_session_skips_the_next_question() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "exec", &json!({"command": "ls"}))
        .with_text_turn("first")
        .with_tool_call_turn("c2", "exec", &json!({"command": "ls"}))
        .with_text_turn("second");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let first = submit(&mut run, "one", OnApproval::ForSession).await;
    assert_eq!(
        first
            .iter()
            .filter(|e| matches!(e, Event::ApprovalRequested { .. }))
            .count(),
        1
    );
    let second = submit(&mut run, "two", OnApproval::Deny).await;
    assert_eq!(final_state(&second), TaskState::Completed);
    assert!(
        !second
            .iter()
            .any(|e| matches!(e, Event::ApprovalRequested { .. }))
    );
    assert_eq!(tool_results(&fake.requests()[3])[0].2, "ran");
    assert_eq!(run.policy.grants()[0].2, harness_ports::GrantScope::Session);
    assert_invariants(&fake);
}

#[tokio::test]
async fn always_grants_are_remembered_with_their_scope() {
    let fake = FakeProvider::new()
        .with_tool_call_turn("c1", "exec", &json!({"command": "make"}))
        .with_text_turn("done");
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::AlwaysUser).await;
    assert_eq!(final_state(&events), TaskState::Completed);
    assert_eq!(run.policy.grants()[0].2, harness_ports::GrantScope::User);
}

// ---------- parallel read-only calls (P2-T7) ----------

#[tokio::test]
async fn parallel_safe_calls_run_together_within_the_limit_and_keep_call_order() {
    let calls = [
        ("c1", "slow_read", json!({"text": "r1"})),
        ("c2", "slow_read", json!({"text": "r2"})),
        ("c3", "slow_read", json!({"text": "r3"})),
        ("c4", "slow_read", json!({"text": "r4"})),
    ];
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&calls)
        .with_text_turn("done");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let started = std::time::Instant::now();
    submit(&mut run, "go", OnApproval::Deny).await;
    let elapsed = started.elapsed();
    // Four 150 ms reads, two at a time: about 300 ms, far below 600 ms sequential.
    assert!(elapsed < Duration::from_millis(550), "{elapsed:?}");
    assert_eq!(run.peak.load(std::sync::atomic::Ordering::SeqCst), 2);
    let results = tool_results(&fake.requests()[1]);
    let order: Vec<(&str, &str)> = results
        .iter()
        .map(|(id, _, t)| (id.as_str(), t.as_str()))
        .collect();
    assert_eq!(
        order,
        [("c1", "r1"), ("c2", "r2"), ("c3", "r3"), ("c4", "r4")]
    );
    assert_invariants(&fake);
}

#[tokio::test]
async fn a_call_needing_approval_splits_parallel_batches_in_order() {
    let calls = [
        ("c1", "slow_read", json!({"text": "a"})),
        ("c2", "exec", json!({"command": "ls"})),
        ("c3", "slow_read", json!({"text": "b"})),
    ];
    let fake = FakeProvider::new()
        .with_tool_calls_turn(&calls)
        .with_text_turn("done");
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10).await;
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    let started: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            Event::ToolStarted { call_id } => Some(call_id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["c1", "c2", "c3"]);
    let ids: Vec<String> = tool_results(&fake.requests()[1])
        .into_iter()
        .map(|r| r.0)
        .collect();
    assert_eq!(ids, ["c1", "c2", "c3"]);
    assert_invariants(&fake);
}
