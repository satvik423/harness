//! The `harness` command.
//!
//! - `harness` prints help.
//! - `harness run PROMPT` runs one prompt headless (Anthropic or OpenAI model) and exits with the codes in
//!   Tech spec 3.16 (0 done, 1 error, 2 budget, 3 refused, 4 approval needed, 130 interrupted).
#![forbid(unsafe_code)]

use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use clap::{Args, CommandFactory, Parser, Subcommand};
use harness_cli::render::exit;
use harness_cli::run::{ApprovalArg, Io, RunOptions, run_session};
use harness_cli::secrets::SystemSecrets;
use harness_cli::telemetry::{self, LOG_ENV};
use harness_cli::wiring::{PolicyInputs, build_policy, build_provider};
use harness_config::{ConfigLoader, Paths};
use harness_policy::ApprovalPolicy;
use harness_ports::SystemClock;
use harness_runtime::RuntimeDeps;
use harness_tools::{MemoryBlobStore, MemoryReadLedger, Unsandboxed};
use harness_types::ModelRef;
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
}

#[derive(Debug, Args)]
struct RunArgs {
    /// What to ask the agent.
    prompt: String,
    /// Print every event as a JSON line on stdout.
    #[arg(long)]
    json: bool,
    /// Model to use, for example openai/gpt-5.5 (default: `default_model` in the config).
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

fn main() -> ExitCode {
    match real_main() {
        Ok(code) => code,
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "error: {e:#}");
            ExitCode::from(exit::ERROR)
        }
    }
}

fn real_main() -> anyhow::Result<ExitCode> {
    let cli = Cli::parse();
    let paths = Paths::discover()?;
    let filter = telemetry::filter_from(std::env::var(LOG_ENV).ok().as_deref())?;
    let _guard = telemetry::init(&paths.log_dir(), filter).context("setting up logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "harness started");

    let Some(Command::Run(args)) = cli.command else {
        Cli::command().print_help()?;
        return Ok(ExitCode::SUCCESS);
    };

    let cwd = std::env::current_dir().context("reading the current directory")?;
    let loaded = ConfigLoader::new(paths.clone())
        .project_dir(&cwd)
        .env(std::env::vars())
        .load()?;
    let mut persona = harness_context::builtin_default();
    persona.model = args
        .model
        .unwrap_or_else(|| loaded.config.default_model.clone());

    // Read the keyring before the async runtime starts: the keyring client blocks.
    let provider = build_provider(&persona.model, &loaded.config, &SystemSecrets)?;
    let home = std::env::home_dir();
    let policy = build_policy(&PolicyInputs {
        config: &loaded.config.policy,
        paths: &paths,
        project: &cwd,
        trusted: loaded.project_trusted,
        approval: args.approval.map(|a| match a {
            ApprovalArg::OnRequest => ApprovalPolicy::OnRequest,
            ApprovalArg::Never => ApprovalPolicy::Never,
        }),
        full_access: args.dangerously_full_access,
        sandboxed: false,
        home: home.as_deref(),
    })?;
    if args.dangerously_full_access {
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
        blobs: Arc::new(MemoryBlobStore::default()),
        ledger: Arc::new(MemoryReadLedger::default()),
        policy,
    };
    let options = RunOptions {
        prompt: args.prompt,
        json: args.json,
        max_turns: args.max_turns,
        output_cap_tokens: loaded.config.context.output_cap_tokens,
        max_parallel_tools: usize::try_from(loaded.config.runtime.max_parallel_tools).unwrap_or(1),
        approval,
        cwd,
    };
    tracing::info!(model = %persona.model, "running prompt");

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
        run_session(options, deps, persona, &mut io, interrupt).await
    })?;
    // A pending stdin read must not keep the process alive.
    runtime.shutdown_timeout(Duration::from_millis(100));
    Ok(ExitCode::from(code))
}
