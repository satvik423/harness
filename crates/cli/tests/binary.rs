//! End-to-end checks of the `harness` binary.

// Test-only file: helpers may unwrap (clippy's test exemption covers only #[test] fns).
#![allow(clippy::unwrap_used)]

use std::path::Path;
use std::process::{Command, Output};

/// Runs the binary with HOME and the working directory in `home`, so nothing
/// touches the real user dirs or creates `.harness/` inside the repository.
fn run(home: &Path, args: &[&str], extra_env: &[(&str, &str)]) -> Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    cmd.args(args)
        .current_dir(home)
        .env("HOME", home)
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("HARNESS_LOG");
    for (k, v) in extra_env {
        cmd.env(k, v);
    }
    cmd.output().expect("the harness binary should start")
}

#[test]
fn version_flag_prints_the_crate_version() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["--version"], &[]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8_lossy(&out.stdout).trim(),
        format!("harness {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn no_arguments_prints_help_and_writes_a_log_file() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &[], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("Usage: harness"));
    // First run with a fresh home: nothing about the log directory may reach the terminal.
    assert!(
        out.stderr.is_empty(),
        "unexpected stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let logs = home.path().join(".local/share/harness/logs");
    let text: String = std::fs::read_dir(&logs)
        .unwrap()
        .map(|e| std::fs::read_to_string(e.unwrap().path()).unwrap())
        .collect();
    assert!(text.contains("harness started"), "{text}");
}

#[test]
fn invalid_log_filter_fails_with_a_clear_message() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &[], &[("HARNESS_LOG", "harness=notalevel")]);
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("HARNESS_LOG"));
}

#[test]
fn run_help_lists_its_flags() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["run", "--help"], &[]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    for flag in ["--json", "--model", "--max-turns", "--approval"] {
        assert!(text.contains(flag), "missing {flag} in\n{text}");
    }
}

#[test]
fn run_without_a_key_explains_how_to_store_one() {
    let home = tempfile::tempdir().unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_harness"));
    let out = cmd
        .args(["run", "hello"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("ANTHROPIC_API_KEY")
        // Point the key at an unset variable so the real keyring is never read.
        .env(
            "HARNESS_PROVIDERS__ANTHROPIC__API_KEY",
            "env:HARNESS_TEST_MISSING_KEY",
        )
        .output()
        .expect("the harness binary should start");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no Anthropic API key found") && err.contains("secret-tool store"),
        "{err}"
    );
}

#[test]
fn unknown_providers_are_rejected() {
    let home = tempfile::tempdir().unwrap();
    let out = run(
        home.path(),
        &["run", "--model", "google/gemini", "hello"],
        &[],
    );
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown provider"));
}

#[test]
fn openai_without_a_key_explains_how_to_store_one() {
    let home = tempfile::tempdir().unwrap();
    let out = Command::new(env!("CARGO_BIN_EXE_harness"))
        .args(["run", "--model", "openai/gpt-x", "hello"])
        .current_dir(home.path())
        .env("HOME", home.path())
        .env_remove("XDG_CONFIG_HOME")
        .env_remove("XDG_DATA_HOME")
        .env_remove("OPENAI_API_KEY")
        // Point the key at an unset variable so the real keyring is never read.
        .env(
            "HARNESS_PROVIDERS__OPENAI__API_KEY",
            "env:HARNESS_TEST_MISSING_KEY",
        )
        .output()
        .expect("the harness binary should start");
    assert_eq!(out.status.code(), Some(1));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("no OpenAI API key found") && err.contains("username openai"),
        "{err}"
    );
}

// ---------- session and task commands (P3-T8) ----------

mod stored {
    use std::sync::Arc;

    use harness_ports::{
        BlobStore, EventLog, LoggedEvent, SessionRecord, SessionStatus, SessionStore,
    };
    use harness_protocol::{Envelope, Event, TaskState};
    use harness_store_sqlite::SqliteStore;
    use harness_types::{ContentBlock, Message, Risk, Role, Seq, SessionId, TaskId};

    /// A project whose session died while a tool call was running.
    pub(super) fn crashed_project(dir: &std::path::Path) -> (SessionId, TaskId) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let store = Arc::new(SqliteStore::open(dir).unwrap());
            let id = SessionId::new();
            let task = TaskId::new();
            store
                .create_session(SessionRecord {
                    id,
                    created_at: 0,
                    title: None,
                    persona: "default".into(),
                    cwd: dir.into(),
                    status: SessionStatus::Open,
                    parent: None,
                    forked_at: None,
                    current_segment: 0,
                })
                .await
                .unwrap();
            let mut persona = harness_context::builtin_default();
            persona.model = "openai/gpt-test".parse().unwrap();
            let mut segment = harness_context::SegmentContext::open(harness_context::SegmentSpec {
                persona,
                tools: harness_tools::builtin_registry().unwrap().specs(),
                instructions: None,
                environment: "<environment>test</environment>".into(),
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
                .push_user(vec![ContentBlock::text("count the files")])
                .unwrap();
            let assistant = Message::new(
                Role::Assistant,
                vec![ContentBlock::ToolCall {
                    id: "c1".into(),
                    name: "shell".into(),
                    input: serde_json::json!({"command": "ls | wc -l"}),
                }],
            );
            let events = vec![
                Event::SessionOpened {
                    persona: "default".into(),
                    segment: 0,
                },
                Event::SegmentOpened {
                    segment: 0,
                    opener: Message::user_text("<environment>test</environment>"),
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
                    name: "shell".into(),
                    input: serde_json::json!({"command": "ls | wc -l"}),
                    risk: Risk::Exec {
                        command: "ls | wc -l".into(),
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
        })
    }
}

#[test]
fn session_list_in_an_empty_project_says_how_to_start() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["session", "list"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("No sessions"));
    assert!(
        !home.path().join(".harness").exists(),
        "looking must not create state"
    );
}

#[test]
fn stored_sessions_can_be_listed_shown_and_exported_by_prefix() {
    let home = tempfile::tempdir().unwrap();
    let (session, task) = stored::crashed_project(home.path());
    let id = session.to_string();

    let list = run(home.path(), &["session", "list"], &[]);
    let text = String::from_utf8_lossy(&list.stdout);
    assert!(
        text.contains(&id) && text.contains("count the files") && text.contains("running"),
        "{text}"
    );

    let show = run(home.path(), &["session", "show", &id[..20]], &[]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(show.status.success(), "{show:?}");
    assert!(
        text.contains("tool_started") && text.contains("shell: ls | wc -l"),
        "{text}"
    );

    let export = run(
        home.path(),
        &["session", "export", &id, "--format", "jsonl"],
        &[],
    );
    assert!(export.status.success(), "{export:?}");
    let file = home.path().join(format!(".harness/exports/{id}.jsonl"));
    assert_eq!(std::fs::read_to_string(file).unwrap().lines().count(), 8);

    let tasks = run(home.path(), &["task", "list"], &[]);
    assert!(String::from_utf8_lossy(&tasks.stdout).contains(&task.to_string()));
}

#[test]
fn pausing_a_task_that_no_process_runs_explains_recovery() {
    let home = tempfile::tempdir().unwrap();
    let (_, task) = stored::crashed_project(home.path());
    let out = run(home.path(), &["task", "pause", &task.to_string()], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("recovered as paused"));
}

#[test]
fn cancelling_a_crashed_task_recovers_it_then_cancels_it_without_a_model() {
    let home = tempfile::tempdir().unwrap();
    let (session, task) = stored::crashed_project(home.path());
    let out = run(home.path(), &["task", "cancel", &task.to_string()], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tasks = run(
        home.path(),
        &["task", "list", "--session", &session.to_string()],
        &[],
    );
    assert!(String::from_utf8_lossy(&tasks.stdout).contains("cancelled"));
    // The unfinished call was reported, not re-run.
    let show = run(home.path(), &["session", "show", &session.to_string()], &[]);
    let text = String::from_utf8_lossy(&show.stdout);
    assert!(
        text.contains("c1 failed The harness stopped while this was running"),
        "{text}"
    );
}

#[test]
fn deleting_a_session_asks_for_yes_then_removes_it() {
    let home = tempfile::tempdir().unwrap();
    let (session, _) = stored::crashed_project(home.path());
    let id = session.to_string();

    // No terminal to confirm on, and no --yes: nothing is deleted.
    let refused = run(home.path(), &["session", "delete", &id[..20]], &[]);
    assert_eq!(refused.status.code(), Some(1), "{refused:?}");
    assert!(String::from_utf8_lossy(&refused.stderr).contains("--yes"));
    let list = run(home.path(), &["session", "list"], &[]);
    assert!(String::from_utf8_lossy(&list.stdout).contains(&id));

    let deleted = run(home.path(), &["session", "delete", &id, "--yes"], &[]);
    assert!(deleted.status.success(), "{deleted:?}");
    let text = String::from_utf8_lossy(&deleted.stdout);
    assert!(
        text.contains(&format!("deleted session {id}")) && text.contains("8 events"),
        "{text}"
    );
    let list = run(home.path(), &["session", "list"], &[]);
    assert!(String::from_utf8_lossy(&list.stdout).contains("No sessions"));
}

#[test]
fn unknown_or_ambiguous_ids_are_errors() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["session", "show", "ZZZZ"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no sessions in this project"));
    stored::crashed_project(home.path());
    let out = run(home.path(), &["session", "show", "ZZZZ"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no session starts with"));
}

// ---------- debug commands (P4-T8) ----------

#[test]
fn debug_last_request_prints_the_rebuilt_provider_body() {
    let home = tempfile::tempdir().unwrap();
    let (session, _) = stored::crashed_project(home.path());
    let out = run(home.path(), &["debug", "last-request"], &[]);
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.starts_with(&format!("// session {session}")), "{text}");
    assert!(
        text.contains("\"model\": \"gpt-test\"") && text.contains("count the files"),
        "{text}"
    );
    let out = run(home.path(), &["debug", "diff-requests"], &[]);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("fewer than two requests"));
}

#[test]
fn bare_harness_without_a_terminal_explains_the_tui_needs_one() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &[], &[]);
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(
        text.contains("Usage:") && text.contains("needs a terminal"),
        "{text}"
    );
}

#[test]
fn continue_is_a_tui_flag_that_excludes_session() {
    let home = tempfile::tempdir().unwrap();
    let out = run(home.path(), &["-c"], &[]);
    assert!(out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stdout).contains("needs a terminal"));
    let help = String::from_utf8_lossy(&run(home.path(), &["--help"], &[]).stdout).into_owned();
    assert!(help.contains("--continue"), "{help}");
    let both = run(home.path(), &["-c", "--session", "01J"], &[]);
    assert_eq!(both.status.code(), Some(2), "{both:?}");
}
