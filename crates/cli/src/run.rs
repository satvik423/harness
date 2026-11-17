//! `harness run`: one prompt, headless, through a real session.

use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use harness_config::{AnthropicConfig, MismatchBehavior, OpenAiConfig, ServerFallbacks};
use harness_context::ActivePersona;
use harness_protocol::{Decision, Event, InputItem, Op};
use harness_provider_anthropic::{AnthropicSettings, ThinkingMismatch};
use harness_provider_openai::OpenAiSettings;
use harness_runtime::{ApprovalMode, RuntimeDeps, SessionConfig, spawn_session};
use harness_types::TaskId;
use tokio::io::{AsyncBufRead, AsyncBufReadExt};
use tokio::sync::{Notify, broadcast};

use crate::prompt::{APPROVAL_CHOICES, parse_answer, parse_decision};
use crate::render::{Outcome, Renderer};

/// Approval policy flag of `harness run`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ApprovalArg {
    /// Ask on the terminal for every call that is not read-only.
    OnRequest,
    /// Deny such calls without asking (scripts and CI).
    Never,
}

/// Options for one `harness run`.
#[derive(Debug, Clone)]
pub struct RunOptions {
    /// The prompt.
    pub prompt: String,
    /// Print every event as a JSON line instead of human output.
    pub json: bool,
    /// Maximum model calls.
    pub max_turns: u32,
    /// Tool output cap in estimated tokens (`context.output_cap_tokens`).
    pub output_cap_tokens: u64,
    /// Parallel-safe tool calls run at once, at most (`runtime.max_parallel_tools`).
    pub max_parallel_tools: usize,
    /// The effective approval policy (from the profile or `--approval`).
    pub approval: ApprovalArg,
    /// Working directory.
    pub cwd: PathBuf,
}

/// Terminal streams, injectable for tests.
pub struct Io {
    /// Where the answer (or JSON lines) goes.
    pub out: Box<dyn Write + Send>,
    /// Where activity, prompts and errors go.
    pub err: Box<dyn Write + Send>,
    /// Where approval answers are read from.
    pub input: Box<dyn AsyncBufRead + Unpin + Send>,
    /// Repeat each answer on `err` after reading it. Set when the input is not
    /// a terminal (piped answers are not echoed by the terminal).
    pub echo_input: bool,
}

/// Maps the config's Anthropic section to adapter settings.
#[must_use]
pub fn anthropic_settings(config: &AnthropicConfig) -> AnthropicSettings {
    AnthropicSettings {
        api_version: config.api_version.clone(),
        betas: config.betas.clone(),
        server_fallbacks: config.server_fallbacks == ServerFallbacks::Default,
        thinking_mismatch: match config.preserved_thinking_mismatch {
            MismatchBehavior::Error => ThinkingMismatch::Error,
            MismatchBehavior::DropBlock => ThinkingMismatch::DropBlock,
        },
        ..AnthropicSettings::default()
    }
}

/// Maps the config's OpenAI section to adapter settings.
#[must_use]
pub fn openai_settings(config: &OpenAiConfig) -> OpenAiSettings {
    OpenAiSettings {
        store: config.store,
        ..OpenAiSettings::default()
    }
}

/// Runs one prompt to completion and returns the exit code.
///
/// `interrupt` is notified on Ctrl-C; the task is interrupted and the run ends with
/// [`crate::render::exit::INTERRUPTED`].
///
/// # Errors
/// Only I/O errors writing output; task failures are reported through the exit code.
pub async fn run_session(
    options: RunOptions,
    deps: RuntimeDeps,
    persona: ActivePersona,
    io: &mut Io,
    interrupt: Arc<Notify>,
) -> std::io::Result<u8> {
    let approval = match options.approval {
        ApprovalArg::OnRequest => ApprovalMode::Ask,
        ApprovalArg::Never => ApprovalMode::Never,
    };
    let config = SessionConfig {
        cwd: options.cwd.clone(),
        persona,
        approval,
        max_turns: options.max_turns,
        output_cap_tokens: options.output_cap_tokens,
        max_parallel_tools: options.max_parallel_tools,
    };
    let (handle, mut events) = spawn_session(deps, config);
    let mut renderer = Renderer::new(options.json);
    let mut outcome = Outcome::default();
    let mut answers = (&mut io.input).lines();
    let mut task: Option<TaskId> = None;
    let mut interrupted = false;

    handle
        .send(Op::Submit {
            session: handle.id(),
            input: vec![InputItem::Text {
                text: options.prompt.clone(),
            }],
        })
        .await
        .map_err(std::io::Error::other)?;

    loop {
        let received = tokio::select! {
            received = events.recv() => received,
            () = interrupt.notified(), if !interrupted => {
                interrupted = true;
                if let Some(task) = task {
                    let _ = handle.send(Op::Interrupt { task }).await;
                }
                continue;
            }
        };
        let envelope = match received {
            Ok(envelope) => envelope,
            Err(broadcast::error::RecvError::Lagged(n)) => {
                writeln!(
                    io.err,
                    "warning: the display fell behind; {n} events were skipped"
                )?;
                continue;
            }
            Err(broadcast::error::RecvError::Closed) => break,
        };
        renderer.render(&envelope, &mut *io.out, &mut *io.err)?;
        outcome.observe(&envelope.event);
        match &envelope.event {
            Event::UserInput { task: t, .. } => {
                task = Some(*t);
                if interrupted {
                    let _ = handle.send(Op::Interrupt { task: *t }).await;
                }
            }
            Event::ApprovalRequested { id, reason, .. } => {
                let prompt =
                    (!options.json).then(|| format!("Allow {reason}? {APPROVAL_CHOICES}: "));
                match read_answer(
                    &mut *io.err,
                    &mut answers,
                    prompt,
                    io.echo_input,
                    &interrupt,
                )
                .await?
                {
                    Answer::Line(line) => {
                        let decision = parse_decision(&line);
                        let _ = handle
                            .send(Op::Approve {
                                request: *id,
                                decision,
                            })
                            .await;
                    }
                    Answer::Closed => {
                        let _ = handle
                            .send(Op::Approve {
                                request: *id,
                                decision: Decision::Deny {
                                    reason: Some("no answer (input closed)".into()),
                                },
                            })
                            .await;
                    }
                    Answer::Interrupted => {
                        interrupted = true;
                        if let Some(task) = task {
                            let _ = handle.send(Op::Interrupt { task }).await;
                        }
                    }
                }
            }
            Event::QuestionAsked {
                id,
                question,
                options: choices,
                multi,
                ..
            } => {
                let prompt = (!options.json).then(|| {
                    let mut text = format!("? {question}\n");
                    for (i, choice) in choices.iter().enumerate() {
                        text.push_str(&format!("  {}. {choice}\n", i + 1));
                    }
                    text.push_str(match (choices.is_empty(), multi) {
                        (true, _) => "answer: ",
                        (false, false) => "answer (a number, or your own words): ",
                        (false, true) => "answer (numbers like 1,3, or your own words): ",
                    });
                    text
                });
                let answer = match read_answer(
                    &mut *io.err,
                    &mut answers,
                    prompt,
                    io.echo_input,
                    &interrupt,
                )
                .await?
                {
                    Answer::Line(line) => parse_answer(&line, choices, *multi),
                    Answer::Closed => "(no answer: the input was closed)".into(),
                    Answer::Interrupted => {
                        interrupted = true;
                        if let Some(task) = task {
                            let _ = handle.send(Op::Interrupt { task }).await;
                        }
                        continue;
                    }
                };
                let _ = handle
                    .send(Op::Answer {
                        request: *id,
                        answer,
                    })
                    .await;
            }
            _ => {}
        }
        if outcome.finished() {
            break;
        }
    }
    let _ = handle.send(Op::Shutdown).await;
    Ok(outcome.exit_code())
}

/// What came back from a prompt.
enum Answer {
    /// A line of input.
    Line(String),
    /// The input was closed (end of a pipe).
    Closed,
    /// Ctrl-C while waiting.
    Interrupted,
}

/// Shows `prompt` (if any) and reads one line, echoing it when `echo` is set.
async fn read_answer<R: AsyncBufRead + Unpin>(
    err: &mut dyn Write,
    answers: &mut tokio::io::Lines<R>,
    prompt: Option<String>,
    echo: bool,
    interrupt: &Notify,
) -> std::io::Result<Answer> {
    let shown = prompt.is_some();
    if let Some(prompt) = prompt {
        write!(err, "{prompt}")?;
        err.flush()?;
    }
    let line = tokio::select! {
        line = answers.next_line() => line?,
        () = interrupt.notified() => return Ok(Answer::Interrupted),
    };
    match line {
        Some(line) => {
            // A terminal echoes what the user typed, including the newline;
            // piped input does not, so print it to keep the output readable.
            if shown && echo {
                writeln!(err, "{line}")?;
            }
            Ok(Answer::Line(line))
        }
        None => {
            if shown {
                writeln!(err)?;
            }
            Ok(Answer::Closed)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::exit;
    use harness_ports::{SystemClock, ToolRegistry};
    use harness_testkit::FakeProvider;
    use harness_tools::{MemoryBlobStore, MemoryReadLedger, Unsandboxed};
    use serde_json::json;
    use std::sync::Mutex;

    /// A `Write` that keeps what it receives in shared memory.
    #[derive(Clone, Default)]
    struct Buffer(Arc<Mutex<Vec<u8>>>);

    impl Write for Buffer {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Buffer {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
        }
    }

    struct Outputs {
        out: Buffer,
        err: Buffer,
    }

    /// The real policy engine with the default config, rooted in a temp dir.
    fn test_policy(dir: &std::path::Path, approval: ApprovalArg) -> Arc<dyn harness_ports::Policy> {
        let paths = harness_config::Paths {
            user_config_dir: dir.join(".test-config"),
            user_data_dir: dir.join(".test-data"),
        };
        let config = harness_config::PolicyConfig::default();
        crate::wiring::build_policy(&crate::wiring::PolicyInputs {
            config: &config,
            paths: &paths,
            project: dir,
            trusted: false,
            approval: Some(match approval {
                ApprovalArg::OnRequest => harness_policy::ApprovalPolicy::OnRequest,
                ApprovalArg::Never => harness_policy::ApprovalPolicy::Never,
            }),
            full_access: false,
            sandboxed: false,
            home: None,
        })
        .unwrap()
    }

    async fn run(
        fake: FakeProvider,
        approval: ApprovalArg,
        json: bool,
        answers: &str,
        dir: &std::path::Path,
    ) -> (u8, Outputs) {
        let deps = RuntimeDeps {
            provider: Arc::new(fake),
            tools: harness_tools::builtin_registry().unwrap_or_else(|_| ToolRegistry::new()),
            clock: Arc::new(SystemClock),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::new(MemoryBlobStore::default()),
            ledger: Arc::new(MemoryReadLedger::default()),
            policy: test_policy(dir, approval),
        };
        let (out, err) = (Buffer::default(), Buffer::default());
        let mut io = Io {
            out: Box::new(out.clone()),
            err: Box::new(err.clone()),
            input: Box::new(std::io::Cursor::new(answers.as_bytes().to_vec())),
            echo_input: true,
        };
        let options = RunOptions {
            prompt: "go".into(),
            json,
            max_turns: 10,
            output_cap_tokens: 2_000,
            max_parallel_tools: 4,
            approval,
            cwd: dir.to_path_buf(),
        };
        let code = run_session(
            options,
            deps,
            harness_context::builtin_default(),
            &mut io,
            Arc::new(Notify::new()),
        )
        .await
        .unwrap();
        (code, Outputs { out, err })
    }

    #[tokio::test]
    async fn text_reply_is_printed_and_exits_zero() {
        let dir = tempfile::tempdir().unwrap();
        let (code, o) = run(
            FakeProvider::new().with_text_turn("All good."),
            ApprovalArg::OnRequest,
            false,
            "",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        assert_eq!(o.out.text(), "All good.\n");
    }

    #[tokio::test]
    async fn read_only_tool_runs_without_a_prompt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes.txt"), "hello").unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "read_file", &json!({"path": "notes.txt"}))
            .with_text_turn("It says hello.");
        let (code, o) = run(fake, ApprovalArg::OnRequest, false, "", dir.path()).await;
        assert_eq!(code, exit::OK);
        assert!(!o.err.text().contains("Allow"), "{}", o.err.text());
        assert!(
            o.err.text().contains("▸ read_file: notes.txt"),
            "{}",
            o.err.text()
        );
    }

    #[tokio::test]
    async fn approved_shell_command_runs() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "echo approved-run"}))
            .with_text_turn("Done.");
        let (code, o) = run(
            fake.clone(),
            ApprovalArg::OnRequest,
            false,
            "y\n",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        assert!(
            o.err.text().contains(
                "Allow run `echo approved-run` (commands run without a sandbox)? [y]es once"
            ),
            "{}",
            o.err.text()
        );
        // Piped answers are echoed, so the result line starts on its own line.
        assert!(o.err.text().contains("(n <why>): y\n"), "{}", o.err.text());
        let second = &fake.requests()[1];
        assert!(
            serde_json::to_string(second)
                .unwrap()
                .contains("approved-run")
        );
    }

    #[tokio::test]
    async fn anything_but_yes_denies() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "touch nope"}))
            .with_text_turn("ok");
        let (code, _) = run(fake, ApprovalArg::OnRequest, false, "no\n", dir.path()).await;
        assert_eq!(code, exit::OK);
        assert!(!dir.path().join("nope").exists());
    }

    #[tokio::test]
    async fn closed_stdin_denies_instead_of_hanging() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "touch nope"}))
            .with_text_turn("ok");
        let (code, _) = run(fake, ApprovalArg::OnRequest, false, "", dir.path()).await;
        assert_eq!(code, exit::OK);
        assert!(!dir.path().join("nope").exists());
    }

    #[tokio::test]
    async fn approval_never_exits_four() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "ls"}))
            .with_text_turn("ok");
        let (code, _) = run(fake, ApprovalArg::Never, false, "", dir.path()).await;
        assert_eq!(code, exit::APPROVAL_REQUIRED);
    }

    #[tokio::test]
    async fn json_mode_emits_parseable_envelopes() {
        let dir = tempfile::tempdir().unwrap();
        let (code, o) = run(
            FakeProvider::new().with_text_turn("hi"),
            ApprovalArg::OnRequest,
            true,
            "",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        let events: Vec<harness_protocol::Envelope> = o
            .out
            .text()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert!(matches!(
            events.first().map(|e| &e.event),
            Some(Event::SessionOpened { .. })
        ));
        assert!(matches!(
            events.last().map(|e| &e.event),
            Some(Event::TaskState { .. })
        ));
    }

    #[tokio::test]
    async fn questions_are_answered_from_the_input() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn(
                "q1",
                "ask_user",
                &json!({"question": "Which colour?", "options": ["red", "blue"]}),
            )
            .with_text_turn("Blue.");
        let (code, o) = run(
            fake.clone(),
            ApprovalArg::OnRequest,
            false,
            "2\n",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        let err = o.err.text();
        assert!(
            err.contains("? Which colour?\n  1. red\n  2. blue\n"),
            "{err}"
        );
        assert!(
            serde_json::to_string(&fake.requests()[1])
                .unwrap()
                .contains("The user answered: blue")
        );
    }

    #[tokio::test]
    async fn a_denial_reason_reaches_the_model() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "make all"}))
            .with_text_turn("ok");
        run(
            fake.clone(),
            ApprovalArg::OnRequest,
            false,
            "n use make test\n",
            dir.path(),
        )
        .await;
        assert!(
            serde_json::to_string(&fake.requests()[1])
                .unwrap()
                .contains("The user denied this action: use make test")
        );
    }

    #[tokio::test]
    async fn approving_for_the_session_is_not_asked_again() {
        let dir = tempfile::tempdir().unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "echo once"}))
            .with_tool_call_turn("c2", "shell", &json!({"command": "echo once"}))
            .with_text_turn("ok");
        let (code, o) = run(
            fake.clone(),
            ApprovalArg::OnRequest,
            false,
            "s\n",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        assert_eq!(
            o.err.text().matches("Allow run").count(),
            1,
            "{}",
            o.err.text()
        );
        assert!(
            serde_json::to_string(&fake.requests()[2])
                .unwrap()
                .contains("[exit code 0]")
        );
    }

    #[tokio::test]
    async fn the_default_deny_rule_on_env_files_is_respected() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".env"), "SECRET=hunter2").unwrap();
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "read_file", &json!({"path": ".env"}))
            .with_tool_call_turn(
                "c2",
                "write_file",
                &json!({"path": ".env.local", "content": "X=1"}),
            )
            .with_tool_call_turn("c3", "shell", &json!({"command": "cat .env"}))
            .with_text_turn("I could not read it.");
        let (code, o) = run(fake.clone(), ApprovalArg::OnRequest, false, "", dir.path()).await;
        assert_eq!(code, exit::OK);
        assert!(
            !o.err.text().contains("Allow"),
            "never asked: {}",
            o.err.text()
        );
        let all = serde_json::to_string(&fake.requests()).unwrap();
        assert!(!all.contains("hunter2"), "the secret leaked into a request");
        // Each request repeats the conversation; the last one holds all three results.
        let last = serde_json::to_string(fake.requests().last().unwrap()).unwrap();
        assert_eq!(last.matches("Denied by policy").count(), 3, "{last}");
        assert!(!dir.path().join(".env.local").exists());
    }

    /// Copies `fixtures/projects/<name>` into `dest`.
    fn copy_fixture(name: &str, dest: &std::path::Path) {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/projects")
            .join(name);
        for entry in std::fs::read_dir(&src).unwrap() {
            let entry = entry.unwrap();
            let to = dest.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                std::fs::create_dir_all(&to).unwrap();
                for inner in std::fs::read_dir(entry.path()).unwrap() {
                    let inner = inner.unwrap();
                    std::fs::copy(inner.path(), to.join(inner.file_name())).unwrap();
                }
            } else {
                std::fs::copy(entry.path(), to).unwrap();
            }
        }
    }

    /// Phase 2 exit criterion, scripted: the model runs the tests, reads the
    /// code, edits it and re-runs the tests, with fewer than 3 approvals.
    #[tokio::test]
    async fn fixes_the_seeded_bug_with_fewer_than_three_approvals() {
        let dir = tempfile::tempdir().unwrap();
        copy_fixture("seeded-bug", dir.path());
        let fake = FakeProvider::new()
            .with_tool_call_turn("c1", "shell", &json!({"command": "sh test.sh"}))
            .with_tool_call_turn("c2", "grep", &json!({"pattern": "average", "mode": "content"}))
            .with_tool_call_turn("c3", "read_file", &json!({"path": "lib/stats.sh"}))
            .with_tool_call_turn(
                "c4",
                "edit_file",
                &json!({"path": "lib/stats.sh", "old": "total / (count + 1)", "new": "total / count"}),
            )
            .with_tool_call_turn("c5", "shell", &json!({"command": "sh test.sh"}))
            .with_text_turn("Fixed the off-by-one in average().");
        let (code, o) = run(
            fake.clone(),
            ApprovalArg::OnRequest,
            false,
            "s\n",
            dir.path(),
        )
        .await;
        assert_eq!(code, exit::OK);
        let approvals = o.err.text().matches("Allow ").count();
        assert!(approvals < 3, "{approvals} approvals:\n{}", o.err.text());
        assert_eq!(approvals, 1, "{}", o.err.text());
        let last = serde_json::to_string(fake.requests().last().unwrap()).unwrap();
        assert!(last.contains("ok:   average of 2 4 6"), "{last}");
        let status = std::process::Command::new("sh")
            .arg("test.sh")
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(status.success(), "the fixture's tests pass after the fix");
    }

    #[test]
    fn openai_settings_follow_the_config() {
        let c = OpenAiConfig {
            store: true,
            ..OpenAiConfig::default()
        };
        assert!(openai_settings(&c).store);
        assert!(!openai_settings(&OpenAiConfig::default()).store);
    }

    #[test]
    fn settings_follow_the_config() {
        let c = AnthropicConfig {
            preserved_thinking_mismatch: MismatchBehavior::Error,
            server_fallbacks: ServerFallbacks::Off,
            betas: vec!["x-1".into()],
            ..AnthropicConfig::default()
        };
        let s = anthropic_settings(&c);
        assert_eq!(s.thinking_mismatch, ThinkingMismatch::Error);
        assert!(!s.server_fallbacks);
        assert_eq!(s.betas, ["x-1"]);
        assert_eq!(s.api_version, "2026-10-21");
    }
}
