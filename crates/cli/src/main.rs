//! The `harness` command.
//!
//! Phase 0: parses arguments (`--help`, `--version`), sets up file logging,
//! and prints help. Commands arrive from Phase 1 (`harness run`) onwards.
#![forbid(unsafe_code)]

use anyhow::Context;
use clap::{CommandFactory, Parser};
use harness_cli::telemetry::{self, LOG_ENV};

/// A general-purpose, persona-driven agent harness.
#[derive(Debug, Parser)]
#[command(name = "harness", version, about, long_about = None)]
struct Cli {}

fn main() -> anyhow::Result<()> {
    let _cli = Cli::parse();
    let paths = harness_config::Paths::discover()?;
    let filter = telemetry::filter_from(std::env::var(LOG_ENV).ok().as_deref())?;
    let _guard = telemetry::init(&paths.log_dir(), filter).context("setting up logging")?;
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "harness started");
    Cli::command().print_help()?;
    Ok(())
}
