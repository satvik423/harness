//! The `shell` tool: run a command with `/bin/sh -c` (Tech spec 3.5).

use std::os::unix::process::ExitStatusExt;
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use harness_ports::{
    CommandSpec, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
};
use harness_protocol::Event;
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::mpsc;

/// Default timeout in seconds.
pub const DEFAULT_TIMEOUT_S: u64 = 120;
/// Maximum timeout in seconds.
pub const MAX_TIMEOUT_S: u64 = 600;
/// Memory safety limit: output beyond this many bytes keeps only its head and
/// tail. The model-facing cap is applied later by the runtime (FR-TOOL-04).
pub const MAX_KEPT_BYTES: usize = 8 * 1024 * 1024;
/// After the command exits, how long to wait for its pipes to close before
/// killing anything left in its process group (for example a stray background job).
const PIPE_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    command: String,
    timeout_s: Option<u64>,
    /// For the user's approval panel only (ADR-043); never part of the command.
    #[serde(default, rename = "description")]
    _description: Option<String>,
}

/// Runs a shell command in the working directory and returns its combined
/// stdout and stderr plus the exit code.
///
/// The command runs in its own process group, so a timeout or interrupt kills
/// everything it started. Live output is emitted as
/// [`Event::ToolOutput`]. Commands cannot leave background processes running
/// with their output attached; long-running processes get dedicated PTY tools
/// in Phase 2.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shell;

/// Collects command output in bounded memory: the first half of the limit, and
/// a rolling tail of about the other half. A command that prints gigabytes
/// cannot exhaust memory, while ordinary output is kept whole.
#[derive(Debug)]
struct Collected {
    half: usize,
    head: String,
    tail: String,
    omitted: u64,
}

impl Collected {
    fn new(limit: usize) -> Self {
        Self {
            half: limit / 2,
            head: String::new(),
            tail: String::new(),
            omitted: 0,
        }
    }

    fn push_str(&mut self, mut chunk: &str) {
        if self.head.len() < self.half {
            let room = floor_char_boundary(chunk, self.half - self.head.len());
            self.head.push_str(&chunk[..room]);
            chunk = &chunk[room..];
        }
        self.tail.push_str(chunk);
        // Trim only once the tail is twice its share, so trimming stays rare.
        if self.tail.len() > 2 * self.half {
            let drop = ceil_char_boundary(&self.tail, self.tail.len() - self.half);
            self.omitted += drop as u64;
            self.tail.drain(..drop);
        }
    }

    #[cfg(test)]
    fn held_bytes(&self) -> usize {
        self.head.len() + self.tail.len()
    }

    fn finish(self) -> String {
        if self.omitted == 0 {
            self.head + &self.tail
        } else {
            format!(
                "{}\n… [{} bytes omitted by the shell tool] …\n{}",
                self.head, self.omitted, self.tail
            )
        }
    }
}

/// Largest char boundary in `s` that is at most `at`.
fn floor_char_boundary(s: &str, at: usize) -> usize {
    (0..=at.min(s.len()))
        .rev()
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(0)
}

/// Smallest char boundary in `s` that is at least `at`.
fn ceil_char_boundary(s: &str, at: usize) -> usize {
    (at..=s.len())
        .find(|&i| s.is_char_boundary(i))
        .unwrap_or(s.len())
}

/// Kills every process in the group led by `pid` (the shell started with `process_group(0)`).
fn kill_group(pid: Option<u32>) {
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // ESRCH (group already gone) is fine; nothing else can usefully be done on failure.
        let _ = nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGKILL,
        );
    }
}

/// Copies a pipe into `tx` as text, carrying incomplete UTF-8 sequences over to the next read.
async fn pump<R: AsyncRead + Unpin>(mut reader: R, tx: mpsc::Sender<String>) {
    let mut buf = vec![0u8; 8192];
    let mut pending: Vec<u8> = Vec::new();
    loop {
        match reader.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                pending.extend_from_slice(&buf[..n]);
                let valid = match std::str::from_utf8(&pending) {
                    Ok(_) => pending.len(),
                    // Incomplete trailing sequence: keep it for the next read.
                    Err(e) if e.error_len().is_none() => e.valid_up_to(),
                    // Truly invalid bytes: decode lossily now.
                    Err(_) => pending.len(),
                };
                let chunk = String::from_utf8_lossy(&pending[..valid]).into_owned();
                pending.drain(..valid);
                if !chunk.is_empty() && tx.send(chunk).await.is_err() {
                    break;
                }
            }
        }
    }
    if !pending.is_empty() {
        let _ = tx
            .send(String::from_utf8_lossy(&pending).into_owned())
            .await;
    }
}

#[async_trait]
impl Tool for Shell {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("shell").unwrap_or_else(|_| unreachable!("valid literal")),
            description: format!(
                "Run a shell command with /bin/sh in the working directory. Returns combined stdout and stderr \
                 and the exit code. Default timeout {DEFAULT_TIMEOUT_S} s, maximum {MAX_TIMEOUT_S} s. Do not \
                 start long-running servers or interactive programs."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "The command line to run."},
                    "description": {"type": "string", "description": crate::DESCRIPTION_FIELD},
                    "timeout_s": {"type": "integer", "minimum": 1, "maximum": MAX_TIMEOUT_S, "description": "Timeout in seconds."}
                },
                "required": ["command", "description"],
                "additionalProperties": false
            }),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, input: &Value, _ctx: &RiskCtx) -> Risk {
        Risk::Exec {
            command: input["command"].as_str().unwrap_or_default().to_owned(),
        }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let timeout_s = input.timeout_s.unwrap_or(DEFAULT_TIMEOUT_S);
        if !(1..=MAX_TIMEOUT_S).contains(&timeout_s) {
            return Err(ToolError::InvalidInput(format!(
                "timeout_s must be between 1 and {MAX_TIMEOUT_S}"
            )));
        }
        let spec = ctx
            .sandbox
            .prepare(CommandSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), input.command.clone()],
                cwd: ctx.cwd.clone(),
                env: Vec::new(),
                network: true,
            })
            .map_err(|e| ToolError::Refused(e.to_string()))?;

        let mut child = tokio::process::Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| ToolError::Failed(format!("cannot start {}: {e}", spec.program)))?;
        let pid = child.id();

        let (tx, mut rx) = mpsc::channel::<String>(64);
        let mut pumps = Vec::new();
        if let Some(out) = child.stdout.take() {
            pumps.push(tokio::spawn(pump(out, tx.clone())));
        }
        if let Some(err) = child.stderr.take() {
            pumps.push(tokio::spawn(pump(err, tx.clone())));
        }
        drop(tx);

        let emit = |chunk: &str| {
            ctx.events.emit(Event::ToolOutput {
                call_id: ctx.call_id.clone(),
                chunk: chunk.to_owned(),
            });
        };
        let mut output = Collected::new(MAX_KEPT_BYTES);
        let deadline = tokio::time::sleep(Duration::from_secs(timeout_s));
        tokio::pin!(deadline);
        let finished = loop {
            tokio::select! {
                Some(chunk) = rx.recv() => {
                    emit(&chunk);
                    output.push_str(&chunk);
                }
                status = child.wait() => break Ok(status),
                () = &mut deadline => break Err(ToolError::Timeout(timeout_s)),
                () = ctx.cancel.cancelled() => break Err(ToolError::Cancelled),
            }
        };
        let status = match finished {
            Ok(status) => status
                .map_err(|e| ToolError::Failed(format!("waiting for the command failed: {e}")))?,
            Err(e) => {
                kill_group(pid);
                pumps.iter().for_each(tokio::task::JoinHandle::abort);
                return Err(e);
            }
        };

        // The shell has exited; collect what is still in the pipes. If something
        // it left behind keeps them open, kill the group instead of waiting forever.
        let drained = tokio::time::timeout(PIPE_GRACE, async {
            while let Some(chunk) = rx.recv().await {
                emit(&chunk);
                output.push_str(&chunk);
            }
        })
        .await;
        if drained.is_err() {
            kill_group(pid);
            pumps.iter().for_each(tokio::task::JoinHandle::abort);
            while let Ok(chunk) = rx.try_recv() {
                output.push_str(&chunk);
            }
        }

        let mut text = output.finish();
        if !text.is_empty() && !text.ends_with('\n') {
            text.push('\n');
        }
        let summary = match (status.code(), status.signal()) {
            (Some(code), _) => format!("exit code {code}"),
            (None, Some(signal)) => format!("killed by signal {signal}"),
            (None, None) => "exited".to_owned(),
        };
        text.push_str(&format!("[{summary}]"));
        Ok(ToolOutput {
            content: vec![ContentBlock::text(text)],
            summary,
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use std::time::Instant;

    async fn run(
        dir: &std::path::Path,
        input: Value,
    ) -> (Result<ToolOutput, ToolError>, Vec<Event>) {
        let (ctx, sink, _) = ctx(dir);
        let out = Shell.run(input, ctx).await;
        (out, sink.events())
    }

    fn text(out: &ToolOutput) -> String {
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => text.clone(),
            other => panic!("unexpected content {other:?}"),
        }
    }

    #[test]
    fn collected_output_under_the_limit_is_kept_whole() {
        let mut c = Collected::new(100);
        c.push_str("hello ");
        c.push_str("world");
        assert_eq!(c.finish(), "hello world");
    }

    #[test]
    fn collected_output_over_the_limit_keeps_head_and_tail_in_bounded_memory() {
        let mut c = Collected::new(100);
        for i in 0..10_000 {
            c.push_str(&format!("{i:04}\n"));
            assert!(c.held_bytes() <= 300, "{}", c.held_bytes());
        }
        let out = c.finish();
        assert!(out.starts_with("0000\n0001\n"), "{out}");
        assert!(out.ends_with("9998\n9999\n"), "{out}");
        assert!(out.contains("bytes omitted by the shell tool"), "{out}");
    }

    #[test]
    fn collected_output_respects_char_boundaries() {
        let mut c = Collected::new(10);
        for _ in 0..100 {
            c.push_str("ééé");
        }
        let out = c.finish();
        assert!(out.starts_with('é') && out.ends_with('é'), "{out}");
    }

    #[tokio::test]
    async fn returns_combined_output_and_exit_code() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = run(
            dir.path(),
            json!({"command": "echo out; echo err >&2; exit 3"}),
        )
        .await;
        let t = text(&out.unwrap());
        assert!(t.contains("out") && t.contains("err"), "{t}");
        assert!(t.ends_with("[exit code 3]"), "{t}");
    }

    #[tokio::test]
    async fn the_description_is_accepted_and_not_run() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = run(
            dir.path(),
            json!({"command": "echo hi", "description": "Say hi"}),
        )
        .await;
        assert_eq!(text(&out.unwrap()), "hi\n[exit code 0]");
    }

    #[tokio::test]
    async fn runs_in_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = run(dir.path(), json!({"command": "pwd"})).await;
        let name = dir.path().file_name().unwrap().to_str().unwrap();
        assert!(text(&out.unwrap()).contains(name));
    }

    #[tokio::test]
    async fn emits_live_output_events() {
        let dir = tempfile::tempdir().unwrap();
        let (_, events) = run(dir.path(), json!({"command": "echo live"})).await;
        assert!(events.iter().any(|e| matches!(e, Event::ToolOutput { call_id, chunk } if call_id == "call-1" && chunk.contains("live"))), "{events:?}");
    }

    #[tokio::test]
    async fn timeout_kills_the_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let (out, _) = run(
            dir.path(),
            json!({"command": "sleep 30 & sleep 30", "timeout_s": 1}),
        )
        .await;
        assert_eq!(out.unwrap_err(), ToolError::Timeout(1));
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn cancellation_stops_the_command() {
        let dir = tempfile::tempdir().unwrap();
        let (ctx, _, _) = ctx(dir.path());
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let start = Instant::now();
        assert_eq!(
            Shell
                .run(json!({"command": "sleep 30"}), ctx)
                .await
                .unwrap_err(),
            ToolError::Cancelled
        );
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn leftover_background_job_does_not_hang_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let (out, _) = run(dir.path(), json!({"command": "sleep 30 & echo done"})).await;
        let t = text(&out.unwrap());
        assert!(t.contains("done") && t.ends_with("[exit code 0]"), "{t}");
        assert!(start.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn large_output_is_returned_whole_for_the_runtime_to_cap() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = run(dir.path(), json!({"command": "yes x | head -c 100000"})).await;
        let t = text(&out.unwrap());
        assert!(t.len() > 100_000, "{}", t.len());
        assert!(!t.contains("omitted"));
    }

    #[tokio::test]
    async fn invalid_input_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (out, _) = run(dir.path(), json!({"cmd": "ls"})).await;
        assert!(matches!(out, Err(ToolError::InvalidInput(_))));
        let (out, _) = run(dir.path(), json!({"command": "ls", "timeout_s": 0})).await;
        assert!(matches!(out, Err(ToolError::InvalidInput(_))));
    }

    #[test]
    fn risk_is_exec_with_the_command() {
        let ctx = RiskCtx {
            cwd: "/p".into(),
            writable_roots: vec![],
        };
        assert_eq!(
            Shell.risk(&json!({"command": "rm -rf x"}), &ctx),
            Risk::Exec {
                command: "rm -rf x".into()
            }
        );
        assert!(!Shell.parallel_safe());
    }
}
