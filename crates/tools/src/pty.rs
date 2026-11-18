//! PTY tools (P2-T9, Tech spec 3.5): `pty_start`, `pty_write`, `pty_read` and
//! `pty_kill`, for REPLs, dev servers and interactive programs.
//!
//! A [`PtyManager`] owns the running programs. Each one runs `/bin/sh -c` on its
//! own pseudo-terminal as a session leader, so its process group is its pid and
//! `pty_kill` (or dropping the manager) kills everything it started. Output is
//! read continuously into a bounded buffer and handed out by `pty_read`.
//! Built on `pty-process` (ADR-027).

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use async_trait::async_trait;
use harness_ports::{
    CommandSpec, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::Notify;

/// Programs that may run at once.
pub const MAX_PTY_SESSIONS: usize = 8;
/// Unread output kept per program; older output is dropped first.
pub const MAX_PTY_BUFFER: usize = 1024 * 1024;
/// Longest a single `pty_read` (or the wait after start/write) may wait.
pub const MAX_PTY_WAIT_MS: u64 = 30_000;
const DEFAULT_START_WAIT_MS: u64 = 500;
const DEFAULT_READ_WAIT_MS: u64 = 200;
/// After new output arrives, wait this long for more before returning.
const SETTLE: Duration = Duration::from_millis(50);

/// Unread output of one program.
#[derive(Debug, Default)]
struct Buffer {
    text: String,
    dropped: usize,
    /// Undecoded bytes at the end of the last read (a split UTF-8 character).
    partial: Vec<u8>,
    exited: Option<String>,
}

impl Buffer {
    fn push(&mut self, bytes: &[u8]) {
        self.partial.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.partial) {
            Ok(_) => self.partial.len(),
            // Keep an incomplete character for the next read; replace real errors.
            Err(e) if e.error_len().is_none() => e.valid_up_to(),
            Err(_) => self.partial.len(),
        };
        let chunk: Vec<u8> = self.partial.drain(..valid).collect();
        self.text.push_str(&clean(&String::from_utf8_lossy(&chunk)));
        if self.text.len() > MAX_PTY_BUFFER {
            let cut = self.text.len() - MAX_PTY_BUFFER;
            let cut = (cut..=self.text.len())
                .find(|&i| self.text.is_char_boundary(i))
                .unwrap_or(self.text.len());
            self.text.drain(..cut);
            self.dropped += cut;
        }
    }

    fn take(&mut self) -> String {
        let mut out = std::mem::take(&mut self.text);
        if self.dropped > 0 {
            out = format!("[… {} earlier bytes dropped …]\n{out}", self.dropped);
            self.dropped = 0;
        }
        out
    }
}

/// Removes terminal escape sequences and carriage returns, which are noise for a model.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => match chars.peek() {
                // CSI: ESC [ params final-byte (0x40..=0x7e)
                Some('[') => {
                    chars.next();
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
                // OSC: ESC ] … BEL or ESC \
                Some(']') => {
                    chars.next();
                    while let Some(c) = chars.next() {
                        if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            c => out.push(c),
        }
    }
    out
}

/// One running program.
struct Session {
    command: String,
    pid: Option<u32>,
    writer: tokio::sync::Mutex<pty_process::OwnedWritePty>,
    buffer: Arc<Mutex<Buffer>>,
    changed: Arc<Notify>,
}

impl Session {
    fn kill(&self) {
        if let Some(pid) = self.pid.and_then(|p| i32::try_from(p).ok()) {
            // The program is a session leader, so its group id is its pid.
            let _ = nix::sys::signal::killpg(
                nix::unistd::Pid::from_raw(pid),
                nix::sys::signal::Signal::SIGKILL,
            );
        }
    }

    fn exited(&self) -> Option<String> {
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .exited
            .clone()
    }

    /// Waits up to `wait` for output, then briefly for it to settle, and takes it.
    async fn collect(
        &self,
        wait: Duration,
        cancel: &tokio_util::sync::CancellationToken,
    ) -> String {
        let has_output = || {
            let b = self.buffer.lock().unwrap_or_else(PoisonError::into_inner);
            !b.text.is_empty() || b.exited.is_some()
        };
        if !has_output() {
            tokio::select! {
                () = self.changed.notified() => {}
                () = tokio::time::sleep(wait) => {}
                () = cancel.cancelled() => {}
            }
        }
        // More output often follows immediately (a prompt after a result line).
        loop {
            tokio::select! {
                () = self.changed.notified() => {}
                () = tokio::time::sleep(SETTLE) => break,
            }
        }
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }
}

/// Owns the programs started with `pty_start`. Dropping it kills them all.
#[derive(Default)]
pub struct PtyManager {
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    next: Mutex<u32>,
}

impl std::fmt::Debug for PtyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyManager")
            .field("sessions", &self.lock().len())
            .finish()
    }
}

impl Drop for PtyManager {
    fn drop(&mut self) {
        for session in self.lock().values() {
            session.kill();
        }
    }
}

impl PtyManager {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Arc<Session>>> {
        self.sessions.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn get(&self, id: &str) -> Result<Arc<Session>, ToolError> {
        self.lock().get(id).cloned().ok_or_else(|| {
            ToolError::InvalidInput(format!(
                "no program {id:?} is running; start one with pty_start"
            ))
        })
    }

    fn start(
        &self,
        spec: &CommandSpec,
        command: &str,
    ) -> Result<(String, Arc<Session>), ToolError> {
        if self.lock().len() >= MAX_PTY_SESSIONS {
            return Err(ToolError::Refused(format!(
                "{MAX_PTY_SESSIONS} programs are already running; stop one with pty_kill first"
            )));
        }
        let fail =
            |e: pty_process::Error| ToolError::Failed(format!("cannot start a terminal: {e}"));
        let (pty, pts) = pty_process::open().map_err(fail)?;
        pty.resize(pty_process::Size::new(40, 120)).map_err(fail)?;
        let mut child = pty_process::Command::new(&spec.program)
            .args(&spec.args)
            .current_dir(&spec.cwd)
            .envs(spec.env.iter().map(|(k, v)| (k.as_str(), v.as_str())))
            // Plain output: no colours or cursor movement for the model to wade through.
            .env("TERM", "dumb")
            .kill_on_drop(true)
            .spawn(pts)
            .map_err(fail)?;
        let pid = child.id();
        let (mut reader, writer) = pty.into_split();
        let buffer = Arc::new(Mutex::new(Buffer::default()));
        let changed = Arc::new(Notify::new());
        {
            let buffer = Arc::clone(&buffer);
            let changed = Arc::clone(&changed);
            tokio::spawn(async move {
                let mut chunk = vec![0u8; 8192];
                // The read side ends with EIO once the program and its children exit.
                while let Ok(n @ 1..) = reader.read(&mut chunk).await {
                    buffer
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner)
                        .push(&chunk[..n]);
                    changed.notify_waiters();
                }
                let status = match child.wait().await {
                    Ok(s) => match s.code() {
                        Some(code) => format!("exited with code {code}"),
                        None => "was killed".to_owned(),
                    },
                    Err(e) => format!("ended ({e})"),
                };
                buffer.lock().unwrap_or_else(PoisonError::into_inner).exited = Some(status);
                changed.notify_waiters();
            });
        }
        let id = {
            let mut next = self.next.lock().unwrap_or_else(PoisonError::into_inner);
            *next += 1;
            format!("pty{next}")
        };
        let session = Arc::new(Session {
            command: command.to_owned(),
            pid,
            writer: tokio::sync::Mutex::new(writer),
            buffer,
            changed,
        });
        self.lock().insert(id.clone(), Arc::clone(&session));
        Ok((id, session))
    }

    fn remove(&self, id: &str) -> Option<Arc<Session>> {
        self.lock().remove(id)
    }
}

fn wait_of(ms: Option<u64>, default: u64) -> Duration {
    Duration::from_millis(ms.unwrap_or(default).min(MAX_PTY_WAIT_MS))
}

fn status_line(id: &str, session: &Session) -> String {
    match session.exited() {
        Some(how) => format!("[{id} {how}]"),
        None => format!("[{id} is running: {}]", session.command),
    }
}

fn output(id: &str, session: &Session, text: &str, summary: String) -> ToolOutput {
    let body = if text.is_empty() {
        "(no new output)".to_owned()
    } else {
        text.trim_end_matches('\n').to_owned()
    };
    ToolOutput {
        content: vec![ContentBlock::text(format!(
            "{body}\n{}",
            status_line(id, session)
        ))],
        summary,
        touched: Vec::new(),
    }
}

fn name(literal: &str) -> ToolName {
    ToolName::new(literal).unwrap_or_else(|_| unreachable!("valid literal"))
}

fn parse<T: for<'de> Deserialize<'de>>(input: Value) -> Result<T, ToolError> {
    serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))
}

/// `pty_start`: runs a command on a new pseudo-terminal.
#[derive(Debug, Clone)]
pub struct PtyStart(pub Arc<PtyManager>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StartInput {
    command: String,
    wait_ms: Option<u64>,
    /// For the user's approval panel only (ADR-043).
    #[serde(default, rename = "description")]
    _description: Option<String>,
}

#[async_trait]
impl Tool for PtyStart {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: name("pty_start"),
            description: "Start an interactive or long-running program (a REPL, dev server, \
                watcher) on its own terminal. Returns an id and the first output. Use \
                pty_write to type into it, pty_read for new output, and pty_kill to stop it. \
                Use shell instead for commands that finish on their own."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "command": {"type": "string"},
                    "description": {"type": "string", "description": crate::DESCRIPTION_FIELD},
                    "wait_ms": {"type": "integer", "minimum": 0, "maximum": MAX_PTY_WAIT_MS, "description": "How long to wait for first output (default 500)."}
                },
                "required": ["command", "description"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, input: &Value, _: &RiskCtx) -> Risk {
        Risk::Exec {
            command: input["command"].as_str().unwrap_or_default().to_owned(),
        }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: StartInput = parse(input)?;
        if input.command.trim().is_empty() {
            return Err(ToolError::InvalidInput("the command is empty".into()));
        }
        let spec = ctx
            .sandbox
            .prepare(CommandSpec {
                program: "/bin/sh".into(),
                args: vec!["-c".into(), input.command.clone()],
                cwd: ctx.cwd.clone(),
                env: Vec::new(),
                // Same as `shell` until the sandbox (Phase 13) decides per command.
                network: true,
            })
            .map_err(|e| ToolError::Refused(e.to_string()))?;
        let (id, session) = self.0.start(&spec, &input.command)?;
        let text = session
            .collect(wait_of(input.wait_ms, DEFAULT_START_WAIT_MS), &ctx.cancel)
            .await;
        Ok(output(&id, &session, &text, format!("started {id}")))
    }
}

/// `pty_write`: types into a running program.
#[derive(Debug, Clone)]
pub struct PtyWrite(pub Arc<PtyManager>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteInput {
    id: String,
    input: String,
    wait_ms: Option<u64>,
    /// For the user's approval panel only (ADR-043).
    #[serde(default, rename = "description")]
    _description: Option<String>,
}

#[async_trait]
impl Tool for PtyWrite {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: name("pty_write"),
            description: "Type text into a program started with pty_start, then return its \
                new output. Include \\n to press Enter. Control keys: \\u0003 is Ctrl-C, \\u0004 is Ctrl-D."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "input": {"type": "string"},
                    "description": {"type": "string", "description": crate::DESCRIPTION_FIELD},
                    "wait_ms": {"type": "integer", "minimum": 0, "maximum": MAX_PTY_WAIT_MS, "description": "How long to wait for output (default 200)."}
                },
                "required": ["id", "input", "description"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, input: &Value, _: &RiskCtx) -> Risk {
        // Typing into a program can run anything it can run (`os.system` in a REPL).
        Risk::Exec {
            command: format!(
                "[{}] {}",
                input["id"].as_str().unwrap_or("?"),
                input["input"].as_str().unwrap_or_default().trim_end()
            ),
        }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: WriteInput = parse(input)?;
        let session = self.0.get(&input.id)?;
        if session.exited().is_some() {
            return Ok(output(
                &input.id,
                &session,
                &session.collect(Duration::ZERO, &ctx.cancel).await,
                "program has exited".into(),
            ));
        }
        {
            let mut writer = session.writer.lock().await;
            writer
                .write_all(input.input.as_bytes())
                .await
                .map_err(|e| ToolError::Failed(format!("cannot write to {}: {e}", input.id)))?;
            writer
                .flush()
                .await
                .map_err(|e| ToolError::Failed(format!("cannot write to {}: {e}", input.id)))?;
        }
        let text = session
            .collect(wait_of(input.wait_ms, DEFAULT_READ_WAIT_MS), &ctx.cancel)
            .await;
        Ok(output(
            &input.id,
            &session,
            &text,
            format!("wrote to {}", input.id),
        ))
    }
}

/// `pty_read`: returns a program's output since the last read.
#[derive(Debug, Clone)]
pub struct PtyRead(pub Arc<PtyManager>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadInput {
    id: String,
    wait_ms: Option<u64>,
}

#[async_trait]
impl Tool for PtyRead {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: name("pty_read"),
            description: "Return new output from a program started with pty_start, waiting up \
                to wait_ms (default 200, max 30000) for some to arrive."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "id": {"type": "string"},
                    "wait_ms": {"type": "integer", "minimum": 0, "maximum": MAX_PTY_WAIT_MS}
                },
                "required": ["id"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    // Reading buffered output changes nothing (see ADR-027).
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: Vec::new() }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: ReadInput = parse(input)?;
        let session = self.0.get(&input.id)?;
        let text = session
            .collect(wait_of(input.wait_ms, DEFAULT_READ_WAIT_MS), &ctx.cancel)
            .await;
        Ok(output(
            &input.id,
            &session,
            &text,
            format!("read {}", input.id),
        ))
    }
}

/// `pty_kill`: stops a program and everything it started.
#[derive(Debug, Clone)]
pub struct PtyKill(pub Arc<PtyManager>);

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KillInput {
    id: String,
}

#[async_trait]
impl Tool for PtyKill {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: name("pty_kill"),
            description: "Stop a program started with pty_start, and everything it started. \
                Returns its last output."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {"id": {"type": "string"}},
                "required": ["id"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    // Only stops a program this session started (see ADR-027).
    fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
        Risk::ReadOnly { paths: Vec::new() }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: KillInput = parse(input)?;
        let session = self.0.get(&input.id)?;
        session.kill();
        // Give the reader a moment to record the exit.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while session.exited().is_none() && tokio::time::Instant::now() < deadline {
            tokio::select! {
                () = session.changed.notified() => {}
                () = tokio::time::sleep(Duration::from_millis(50)) => {}
            }
        }
        let text = session.collect(Duration::ZERO, &ctx.cancel).await;
        self.0.remove(&input.id);
        Ok(output(
            &input.id,
            &session,
            &text,
            format!("stopped {}", input.id),
        ))
    }
}

/// The four PTY tools sharing one manager.
#[must_use]
pub fn pty_tools(manager: &Arc<PtyManager>) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(PtyStart(Arc::clone(manager))),
        Arc::new(PtyWrite(Arc::clone(manager))),
        Arc::new(PtyRead(Arc::clone(manager))),
        Arc::new(PtyKill(Arc::clone(manager))),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    fn text(out: &ToolOutput) -> &str {
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => text,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn alive(pid: u32) -> bool {
        nix::sys::signal::killpg(
            nix::unistd::Pid::from_raw(i32::try_from(pid).unwrap()),
            None,
        )
        .is_ok()
    }

    async fn start(m: &Arc<PtyManager>, c: &ToolCtx, command: &str) -> (String, String) {
        let out = PtyStart(Arc::clone(m))
            .run(json!({"command": command}), c.clone())
            .await
            .unwrap();
        let t = text(&out).to_owned();
        let id = m.lock().keys().max().cloned().unwrap();
        (id, t)
    }

    #[tokio::test]
    async fn a_repl_style_program_echoes_what_is_typed() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        let (id, first) = start(&m, &c, "cat").await;
        assert!(first.ends_with("[pty1 is running: cat]"), "{first}");
        let out = PtyWrite(Arc::clone(&m))
            .run(
                json!({"id": id, "input": "hello pty\n", "wait_ms": 2000}),
                c.clone(),
            )
            .await
            .unwrap();
        assert!(text(&out).contains("hello pty"), "{}", text(&out));
        assert!(!text(&out).contains('\r'), "carriage returns are removed");
    }

    #[tokio::test]
    async fn output_is_buffered_between_reads_and_exit_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        let (id, first) = start(&m, &c, "sleep 1; echo later; exit 3").await;
        assert!(first.starts_with("(no new output)"), "{first}");
        let out = PtyRead(Arc::clone(&m))
            .run(json!({"id": id, "wait_ms": 3000}), c.clone())
            .await
            .unwrap();
        let mut seen = text(&out).to_owned();
        // The exit may land in a following read.
        for _ in 0..20 {
            if seen.contains("exited with code 3") {
                break;
            }
            let out = PtyRead(Arc::clone(&m))
                .run(json!({"id": id, "wait_ms": 500}), c.clone())
                .await
                .unwrap();
            seen.push_str(text(&out));
        }
        assert!(seen.contains("later"), "{seen}");
        assert!(seen.contains("[pty1 exited with code 3]"), "{seen}");
    }

    #[tokio::test]
    async fn kill_stops_the_whole_process_group() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        let (id, _) = start(&m, &c, "sleep 30 & sleep 30").await;
        let pid = m.get(&id).unwrap().pid.unwrap();
        assert!(alive(pid));
        let out = PtyKill(Arc::clone(&m))
            .run(json!({"id": id}), c.clone())
            .await
            .unwrap();
        assert!(text(&out).contains("was killed"), "{}", text(&out));
        for _ in 0..40 {
            if !alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive(pid), "process group still alive");
        assert!(m.get(&id).is_err(), "the id is forgotten after kill");
    }

    #[tokio::test]
    async fn dropping_the_manager_kills_its_programs() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        let (id, _) = start(&m, &c, "sleep 30").await;
        let pid = m.get(&id).unwrap().pid.unwrap();
        drop(m);
        for _ in 0..40 {
            if !alive(pid) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        assert!(!alive(pid));
    }

    #[tokio::test]
    async fn unknown_ids_and_empty_commands_are_invalid() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        assert!(matches!(
            PtyRead(Arc::clone(&m))
                .run(json!({"id": "pty9"}), c.clone())
                .await,
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            PtyStart(Arc::clone(&m))
                .run(json!({"command": " "}), c.clone())
                .await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[tokio::test]
    async fn the_number_of_programs_is_limited() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let m = Arc::new(PtyManager::default());
        for _ in 0..MAX_PTY_SESSIONS {
            PtyStart(Arc::clone(&m))
                .run(json!({"command": "sleep 30", "wait_ms": 0}), c.clone())
                .await
                .unwrap();
        }
        let err = PtyStart(Arc::clone(&m))
            .run(json!({"command": "sleep 30", "wait_ms": 0}), c.clone())
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Refused(_)), "{err}");
    }

    #[test]
    fn escape_sequences_and_carriage_returns_are_cleaned() {
        assert_eq!(clean("\u{1b}[1;32mok\u{1b}[0m\r\n"), "ok\n");
        assert_eq!(clean("\u{1b}]0;title\u{7}prompt> "), "prompt> ");
        assert_eq!(clean("a\u{1b}]8;;http://x\u{1b}\\link"), "alink");
        assert_eq!(clean("plain"), "plain");
    }

    #[test]
    fn split_utf8_characters_are_joined_across_reads() {
        let mut b = Buffer::default();
        let bytes = "héllo".as_bytes();
        b.push(&bytes[..2]);
        b.push(&bytes[2..]);
        assert_eq!(b.take(), "héllo");
    }

    #[test]
    fn the_buffer_drops_the_oldest_output_when_full() {
        let mut b = Buffer::default();
        b.push(&vec![b'a'; MAX_PTY_BUFFER]);
        b.push(b"tail");
        let out = b.take();
        assert!(
            out.starts_with("[… 4 earlier bytes dropped …]\n"),
            "{}",
            &out[..40]
        );
        assert!(out.ends_with("tail"));
    }

    #[test]
    fn risks_follow_adr_027() {
        let ctx = RiskCtx {
            cwd: "/".into(),
            writable_roots: vec![],
        };
        let m = Arc::new(PtyManager::default());
        assert_eq!(
            PtyStart(Arc::clone(&m)).risk(&json!({"command": "python3"}), &ctx),
            Risk::Exec {
                command: "python3".into()
            }
        );
        assert_eq!(
            PtyWrite(Arc::clone(&m)).risk(&json!({"id": "pty1", "input": "print(1)\n"}), &ctx),
            Risk::Exec {
                command: "[pty1] print(1)".into()
            }
        );
        assert!(
            PtyRead(Arc::clone(&m))
                .risk(&json!({}), &ctx)
                .is_read_only()
        );
        assert!(PtyKill(m).risk(&json!({}), &ctx).is_read_only());
    }
}
