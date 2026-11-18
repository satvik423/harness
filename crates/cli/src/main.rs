//! The `harness` command.
//!
//! - `harness` prints help.
//! - `harness run PROMPT` runs one prompt headless, in a new session (or an
//!   existing one with `--session`), and exits with the codes in Tech spec 3.16
//!   (0 done, 1 error, 2 budget, 3 refused, 4 approval needed, 130 interrupted).
//! - `harness session list|show|resume|export|undo` and
//!   `harness task list|pause|resume|cancel` manage stored sessions (Phase 3).
#![forbid(unsafe_code)]

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, bail};
use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use harness_cli::render::exit;
use harness_cli::run::{Action, ApprovalArg, Io, Locker, RunOptions, Target, execute};
use harness_cli::secrets::SystemSecrets;
use harness_cli::sessions::{
    event_line, export_jsonl, export_markdown, format_time, resolve_session, resolve_task,
    session_table, task_table,
};
use harness_cli::telemetry::{self, LOG_ENV};
use harness_cli::wiring::{Offline, PolicyInputs, build_policy, build_provider, session_model};
use harness_config::{ConfigLoader, LoadedConfig, Paths};
use harness_policy::ApprovalPolicy;
use harness_ports::{EventLog, Provider, SessionStore, SystemClock, TaskControl};
use harness_protocol::TaskState;
use harness_runtime::RuntimeDeps;
use harness_store_sqlite::SqliteStore;
use harness_tools::{MemoryReadLedger, Unsandboxed};
use harness_types::{ModelRef, Seq, SessionId};
use tokio::sync::Notify;

/// A general-purpose, persona-driven agent harness.
#[derive(Debug, Parser)]
#[command(name = "harness", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run one prompt without the interactive UI, then exit.
    Run(RunArgs),
    /// List, inspect, resume, export or undo stored sessions.
    #[command(subcommand)]
    Session(SessionCommand),
    /// List, pause, resume or cancel tasks.
    #[command(subcommand)]
    Task(TaskCommand),
}

/// Flags for commands that run a model.
#[derive(Debug, Args)]
struct RunFlags {
    /// Print every event as a JSON line on stdout.
    #[arg(long)]
    json: bool,
    /// Model for a new session, for example openai/gpt-5.5 (default: `default_model`
    /// in the config). A resumed session keeps the model it was started with.
    #[arg(long)]
    model: Option<ModelRef>,
    /// Maximum model calls for the task.
    #[arg(long, default_value_t = 50)]
    max_turns: u32,
    /// What to do with actions that need approval (default: from `policy.profile`).
    #[arg(long, value_enum)]
    approval: Option<ApprovalArg>,
    /// Run with the `full` profile: no approvals and no protections except deny
    /// rules. Only for throwaway environments.
    #[arg(long)]
    dangerously_full_access: bool,
}

#[derive(Debug, Args)]
struct RunArgs {
    /// What to ask the agent.
    prompt: String,
    /// Continue an existing session (an id or a unique prefix of one).
    #[arg(long)]
    session: Option<String>,
    #[command(flatten)]
    flags: RunFlags,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ExportFormat {
    /// A readable transcript.
    Md,
    /// Every event as one JSON line.
    Jsonl,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    /// List this project's sessions, newest first.
    List,
    /// Show a session's events.
    Show {
        /// Session id or unique prefix.
        id: String,
    },
    /// Continue a session's paused (or recovered) task.
    Resume {
        /// Session id or unique prefix.
        id: String,
        #[command(flatten)]
        flags: RunFlags,
    },
    /// Write a session to a file (default: .harness/exports/ID.FORMAT).
    Export {
        /// Session id or unique prefix.
        id: String,
        /// Output format.
        #[arg(long, value_enum, default_value_t = ExportFormat::Md)]
        format: ExportFormat,
        /// Output file.
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Restore the files changed by the session's last turn.
    Undo {
        /// Session id or unique prefix.
        id: String,
    },
}

#[derive(Debug, Subcommand)]
enum TaskCommand {
    /// List tasks (of one session, or all).
    List {
        /// Only this session (id or unique prefix).
        #[arg(long)]
        session: Option<String>,
    },
    /// Pause a task at its next safe point.
    Pause {
        /// Task id or unique prefix.
        id: String,
    },
    /// Resume a paused task.
    Resume {
        /// Task id or unique prefix.
        id: String,
        #[command(flatten)]
        flags: RunFlags,
    },
    /// Cancel a task.
    Cancel {
        /// Task id or unique prefix.
        id: String,
    },
}

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: {e:#}");
            ExitCode::from(exit::ERROR)
        }
    }
}

/// What every command needs.
struct App {
    paths: Paths,
    cwd: PathBuf,
    loaded: LoadedConfig,
    store: Arc<SqliteStore>,
}

fn real_main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;
    let filter = telemetry::filter_from(std::env::var(LOG_ENV).ok().as_deref())?;
    let _guard = telemetry::init(&paths.log_dir(), filter).context("setting up logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "harness started");

    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };
    let cwd = std::env::current_dir().context("reading the current directory")?;
    let loaded = ConfigLoader::new(paths.clone())
        .project_dir(&cwd)
        .env(std::env::vars())
        .load()?;
    // Looking around must not create `.harness/` in a folder that has none.
    let read_only = matches!(
        &command,
        Command::Session(
            SessionCommand::List | SessionCommand::Show { .. } | SessionCommand::Export { .. }
        ) | Command::Task(TaskCommand::List { .. } | TaskCommand::Pause { .. })
    );
    if read_only
        && !cwd
            .join(".harness")
            .join(harness_store_sqlite::STATE_DB)
            .exists()
    {
        let lists = matches!(
            &command,
            Command::Session(SessionCommand::List) | Command::Task(TaskCommand::List { .. })
        );
        if lists {
            writeln!(
                std::io::stdout(),
                "No sessions in this project yet. Start one with `harness run PROMPT`."
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        bail!("no sessions in this project yet (no .harness/state.db here)");
    }
    // Opening is blocking file work: do it before any async runtime exists.
    let store = Arc::new(SqliteStore::open(&cwd).context("opening .harness/state.db")?);
    let app = App {
        paths,
        cwd,
        loaded,
        store,
    };
    match command {
        Command::Run(args) => {
            let session = match &args.session {
                Some(prefix) => Some(app.find_session(prefix)?),
                None => None,
            };
            let action = Action::Prompt(args.prompt);
            app.drive(&args.flags, session, action, true)
        }
        Command::Session(cmd) => app.session(cmd),
        Command::Task(cmd) => app.task(cmd),
    }
}

/// Runs a future to completion on a small runtime (for quick store queries).
fn block_on<F: std::future::Future>(future: F) -> anyhow::Result<F::Output> {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    Ok(rt.block_on(future))
}

impl App {
    fn find_session(&self, prefix: &str) -> anyhow::Result<SessionId> {
        let sessions = block_on(self.store.sessions())??;
        Ok(resolve_session(&sessions, prefix)?.id)
    }

    fn session(self, cmd: SessionCommand) -> anyhow::Result<ExitCode> {
        let mut out = std::io::stdout();
        match cmd {
            SessionCommand::List => {
                let store = Arc::clone(&self.store);
                let rows = block_on(async move {
                    let mut rows = Vec::new();
                    for s in store.sessions().await? {
                        let last = store.tasks(Some(s.id)).await?.pop();
                        rows.push((s, last));
                    }
                    Ok::<_, harness_ports::StoreError>(rows)
                })??;
                write!(out, "{}", session_table(&rows))?;
                Ok(ExitCode::SUCCESS)
            }
            SessionCommand::Show { id } => {
                let id = self.find_session(&id)?;
                let store = Arc::clone(&self.store);
                let (record, events, tasks) = block_on(async move {
                    let record = store.session(id).await?;
                    let events = store.range(id, Seq::FIRST, Seq(u64::MAX)).await?;
                    let tasks = store.tasks(Some(id)).await?;
                    Ok::<_, harness_ports::StoreError>((record, events, tasks))
                })??;
                let record = record.context("the session disappeared")?;
                writeln!(
                    out,
                    "session {}\n  title    {}\n  persona  {}\n  created  {} UTC\n  project  {}\n  events   {}\n",
                    record.id,
                    record.title.as_deref().unwrap_or("-"),
                    record.persona,
                    format_time(record.created_at),
                    record.cwd.display(),
                    events.len()
                )?;
                if !tasks.is_empty() {
                    writeln!(out, "{}", task_table(&tasks))?;
                }
                for e in &events {
                    writeln!(out, "{}", event_line(e))?;
                }
                Ok(ExitCode::SUCCESS)
            }
            SessionCommand::Export { id, format, output } => {
                let id = self.find_session(&id)?;
                let store = Arc::clone(&self.store);
                let (record, events) = block_on(async move {
                    let record = store.session(id).await?;
                    let events = store.range(id, Seq::FIRST, Seq(u64::MAX)).await?;
                    Ok::<_, harness_ports::StoreError>((record, events))
                })??;
                let record = record.context("the session disappeared")?;
                let (text, ext) = match format {
                    ExportFormat::Md => (export_markdown(&record, &events), "md"),
                    ExportFormat::Jsonl => (export_jsonl(&events), "jsonl"),
                };
                let path = output.unwrap_or_else(|| {
                    self.store
                        .harness_dir()
                        .join("exports")
                        .join(format!("{id}.{ext}"))
                });
                if let Some(parent) = path.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(&path, text)
                    .with_context(|| format!("writing {}", path.display()))?;
                writeln!(
                    out,
                    "exported {} events to {}",
                    events.len(),
                    path.display()
                )?;
                Ok(ExitCode::SUCCESS)
            }
            SessionCommand::Resume { id, flags } => {
                let id = self.find_session(&id)?;
                self.drive(&flags, Some(id), Action::Resume(None), true)
            }
            SessionCommand::Undo { id } => {
                let id = self.find_session(&id)?;
                self.drive(&RunFlags::offline(), Some(id), Action::Undo, false)
            }
        }
    }

    fn task(self, cmd: TaskCommand) -> anyhow::Result<ExitCode> {
        let mut out = std::io::stdout();
        if let TaskCommand::List { session } = &cmd {
            let session = match session {
                Some(prefix) => Some(self.find_session(prefix)?),
                None => None,
            };
            let tasks = block_on(self.store.tasks(session))??;
            write!(out, "{}", task_table(&tasks))?;
            return Ok(ExitCode::SUCCESS);
        }
        let (TaskCommand::Pause { id }
        | TaskCommand::Resume { id, .. }
        | TaskCommand::Cancel { id }) = &cmd
        else {
            return Ok(ExitCode::SUCCESS);
        };
        let tasks = block_on(self.store.tasks(None))??;
        let task = resolve_task(&tasks, id)?.clone();
        let busy_elsewhere = self.store.is_session_locked(task.session);
        let control = match &cmd {
            TaskCommand::Pause { .. } => TaskControl::Pause,
            TaskCommand::Resume { .. } => TaskControl::Resume,
            _ => TaskControl::Cancel,
        };
        if busy_elsewhere {
            // Another harness process runs this session: leave it a request.
            block_on(self.store.request_control(task.id, control))??;
            writeln!(
                out,
                "requested {control:?} for task {}; the harness running session {} applies it at its next safe point",
                task.id, task.session
            )?;
            return Ok(ExitCode::SUCCESS);
        }
        if task.state.is_terminal() || task.state == TaskState::BudgetExhausted {
            writeln!(out, "task {} has already ended ({:?})", task.id, task.state)?;
            return Ok(ExitCode::SUCCESS);
        }
        match cmd {
            TaskCommand::Pause { .. } => {
                let note = if task.state == TaskState::Paused {
                    "is already paused"
                } else {
                    "is not running in any harness process (harness stopped while it ran); it will be recovered as paused when its session is resumed"
                };
                writeln!(out, "task {} {note}", task.id)?;
                Ok(ExitCode::SUCCESS)
            }
            TaskCommand::Resume { flags, .. } => self.drive(
                &flags,
                Some(task.session),
                Action::Resume(Some(task.id)),
                true,
            ),
            TaskCommand::Cancel { .. } => self.drive(
                &RunFlags::offline(),
                Some(task.session),
                Action::Cancel(task.id),
                false,
            ),
            TaskCommand::List { .. } => Ok(ExitCode::SUCCESS),
        }
    }

    /// Opens a session (new or stored) and performs `action` on it.
    fn drive(
        self,
        flags: &RunFlags,
        session: Option<SessionId>,
        action: Action,
        needs_model: bool,
    ) -> anyhow::Result<ExitCode> {
        let config = &self.loaded.config;
        // A stored session keeps the model its segment was opened with.
        let stored_model = match session {
            Some(id) => block_on(session_model(self.store.as_ref(), self.store.as_ref(), id))??,
            None => None,
        };
        if let (Some(stored), Some(asked)) = (&stored_model, &flags.model)
            && stored != asked
        {
            writeln!(
                std::io::stderr(),
                "note: session uses {stored}; --model {asked} applies to new sessions only"
            )?;
        }
        let mut persona = harness_context::builtin_default();
        persona.model = stored_model
            .or_else(|| flags.model.clone())
            .unwrap_or_else(|| config.default_model.clone());

        // Read the keyring before the main runtime starts: the keyring client blocks.
        let provider: Arc<dyn Provider> = if needs_model {
            build_provider(&persona.model, config, &SystemSecrets)?
        } else {
            Arc::new(Offline)
        };
        let home = std::env::home_dir();
        let policy = build_policy(&PolicyInputs {
            config: &config.policy,
            paths: &self.paths,
            project: &self.cwd,
            trusted: self.loaded.project_trusted,
            approval: flags.approval.map(|a| match a {
                ApprovalArg::OnRequest => ApprovalPolicy::OnRequest,
                ApprovalArg::Never => ApprovalPolicy::Never,
            }),
            full_access: flags.dangerously_full_access,
            sandboxed: false,
            home: home.as_deref(),
        })?;
        if flags.dangerously_full_access {
            writeln!(
                std::io::stderr(),
                "warning: --dangerously-full-access: commands and edits run without approval or sandbox"
            )?;
        }
        let approval = match policy.settings().approval {
            ApprovalPolicy::OnRequest => ApprovalArg::OnRequest,
            ApprovalPolicy::Never => ApprovalArg::Never,
        };
        let deps = RuntimeDeps {
            provider,
            tools: harness_tools::builtin_registry()?,
            clock: Arc::new(SystemClock),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::clone(&self.store) as _,
            ledger: Arc::new(MemoryReadLedger::default()),
            policy,
            log: Arc::clone(&self.store) as _,
            sessions: Arc::clone(&self.store) as _,
            snapshots: Arc::clone(&self.store) as _,
        };
        let options = RunOptions {
            json: flags.json,
            max_turns: flags.max_turns,
            output_cap_tokens: config.context.output_cap_tokens,
            max_parallel_tools: usize::try_from(config.runtime.max_parallel_tools).unwrap_or(1),
            approval,
            cwd: self.cwd.clone(),
        };
        let target = session.map_or(Target::New, Target::Existing);
        tracing::info!(model = %persona.model, ?target, "driving a session");

        let store = Arc::clone(&self.store);
        let locker: Box<Locker> = Box::new(move |id| {
            store
                .lock_session(id)
                .map(|lock| Box::new(lock) as Box<dyn std::any::Any + Send>)
                .map_err(|e| {
                    format!("{e}; use `harness task pause|cancel` to control it from here")
                })
        });
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()?;
        let code = runtime.block_on(async move {
            let interrupt = Arc::new(Notify::new());
            let notify = Arc::clone(&interrupt);
            tokio::spawn(async move {
                // First Ctrl-C interrupts the task; a second one exits immediately.
                let mut presses = 0;
                while tokio::signal::ctrl_c().await.is_ok() {
                    presses += 1;
                    if presses > 1 {
                        std::process::exit(i32::from(exit::INTERRUPTED));
                    }
                    notify.notify_one();
                }
            });
            let mut io = Io {
                out: Box::new(std::io::stdout()),
                err: Box::new(std::io::stderr()),
                input: Box::new(tokio::io::BufReader::new(tokio::io::stdin())),
                echo_input: !std::io::stdin().is_terminal(),
            };
            execute(
                &options,
                deps,
                persona,
                target,
                action,
                locker.as_ref(),
                &mut io,
                interrupt,
            )
            .await
        })?;
        // A pending stdin read must not keep the process alive.
        runtime.shutdown_timeout(Duration::from_millis(100));
        Ok(ExitCode::from(code))
    }
}

impl RunFlags {
    /// Flags for commands that never call a model.
    fn offline() -> Self {
        Self {
            json: false,
            model: None,
            max_turns: 1,
            approval: Some(ApprovalArg::Never),
            dangerously_full_access: false,
        }
    }
}
