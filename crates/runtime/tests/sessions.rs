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
use harness_testkit::FakeProvider;
use harness_testkit::invariants::check_all;
use harness_tools::{MemoryBlobStore, MemoryReadLedger, Unsandboxed};
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
    _dir: tempfile::TempDir,
}

fn start(provider: Arc<dyn Provider>, approval: ApprovalMode, max_turns: u32) -> Run {
    let dir = tempfile::tempdir().unwrap();
    let mut tools = ToolRegistry::new();
    tools.register(Arc::new(Echo)).unwrap();
    tools.register(Arc::new(Exec)).unwrap();
    tools.register(Arc::new(Fails)).unwrap();
    let deps = RuntimeDeps {
        provider,
        tools,
        clock: Arc::new(SystemClock),
        sandbox: Arc::new(Unsandboxed),
        blobs: Arc::new(MemoryBlobStore::default()),
        ledger: Arc::new(MemoryReadLedger::default()),
    };
    let config = SessionConfig {
        cwd: dir.path().to_path_buf(),
        persona: builtin_default(),
        approval,
        max_turns,
    };
    let (handle, events) = spawn_session(deps, config);
    Run {
        handle,
        events,
        _dir: dir,
    }
}

/// What to do when an approval is requested.
#[derive(Clone, Copy)]
enum OnApproval {
    Approve,
    Deny,
    Interrupt,
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
        let event = env.event;
        if let Event::UserInput { task: t, .. } = &event {
            task = Some(*t);
        }
        if let Event::ApprovalRequested { id, .. } = &event {
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
            };
            run.handle.send(op).await.unwrap();
        }
        let done = ends_task(&event);
        events.push(event);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Never, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 2);
    let events = submit(&mut run, "loop", OnApproval::Approve).await;
    assert_eq!(final_state(&events), TaskState::BudgetExhausted);
    assert_eq!(fake.requests().len(), 2);
    assert_invariants(&fake);
}

#[tokio::test]
async fn provider_error_fails_the_task_with_an_error_event() {
    let fake = FakeProvider::new().with_turn(vec![Err(ProviderError::Auth("bad key".into()))]);
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10);
    let session = run.handle.id();
    for op in [
        Op::Pause {
            task: TaskId::new(),
        },
        Op::Resume {
            task: TaskId::new(),
        },
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
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(FakeProvider::new()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake.clone()), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10);
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
    let mut run = start(Arc::new(fake), ApprovalMode::Ask, 10);
    let events = submit(&mut run, "go", OnApproval::Approve).await;
    assert!(events.iter().any(|e| matches!(e, Event::Warning { kind, message } if kind == "provider_notice" && message == "thinking dropped")));
}

#[tokio::test]
async fn shutdown_cancels_a_running_task() {
    let mut run = start(Arc::new(Hanging), ApprovalMode::Ask, 10);
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
