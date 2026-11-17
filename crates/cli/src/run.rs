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
    /// Approval policy.
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
                match ask(&mut *io.err, &mut answers, reason, options.json, &interrupt).await? {
                    Some(decision) => {
                        let _ = handle
                            .send(Op::Approve {
                                request: *id,
                                decision,
                            })
                            .await;
                    }
                    None => {
                        interrupted = true;
                        if let Some(task) = task {
                            let _ = handle.send(Op::Interrupt { task }).await;
                        }
                    }
                }
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

/// Asks the user to approve an action. Returns `None` if interrupted while waiting.
async fn ask<R: AsyncBufRead + Unpin>(
    err: &mut dyn Write,
    answers: &mut tokio::io::Lines<R>,
    reason: &str,
    json: bool,
    interrupt: &Notify,
) -> std::io::Result<Option<Decision>> {
    if !json {
        write!(err, "Allow: {reason}? [y/N] ")?;
        err.flush()?;
    }
    let line = tokio::select! {
        line = answers.next_line() => line?,
        () = interrupt.notified() => return Ok(None),
    };
    let Some(answer) = line else {
        if !json {
            writeln!(err)?;
        }
        return Ok(Some(Decision::Deny {
            reason: Some("no answer (input closed)".into()),
        }));
    };
    let yes = matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    Ok(Some(if yes {
        Decision::Once
    } else {
        Decision::Deny { reason: None }
    }))
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
        };
        let (out, err) = (Buffer::default(), Buffer::default());
        let mut io = Io {
            out: Box::new(out.clone()),
            err: Box::new(err.clone()),
            input: Box::new(std::io::Cursor::new(answers.as_bytes().to_vec())),
        };
        let options = RunOptions {
            prompt: "go".into(),
            json,
            max_turns: 10,
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
            o.err
                .text()
                .contains("Allow: run `echo approved-run`? [y/N]"),
            "{}",
            o.err.text()
        );
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
