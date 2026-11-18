//! `harness-evals`: the context-engine evals (Tech spec 3.18, Phase 5).
//!
//! ```sh
//! harness-evals needle   --model openai/gpt-6-luna             # M1, anchored
//! harness-evals needle   --model openai/gpt-6-luna --baseline  # M1 baseline
//! harness-evals fidelity --model openai/gpt-6-luna             # M2
//! harness-evals needle   --offline                             # no key, no cost
//! ```
//!
//! Each run appends a row to `evals/results.tsv`. Live runs use the API key
//! from the keyring or environment, exactly as `harness` does, and cost money:
//! see `evals/README.md` for estimates.
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod driver;
mod evals;
mod offline;
mod scenario;

use std::io::Write as _;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use clap::{Parser, Subcommand};
use harness_cli::secrets::SystemSecrets;
use harness_cli::wiring::build_provider;
use harness_config::{ConfigLoader, Paths};
use harness_context::compaction::CompactionStyle;
use harness_ports::Provider;
use harness_runtime::ModelPrice;
use harness_types::{Effort, ModelRef};

use crate::driver::Setup;

/// Context-engine evals for harness.
#[derive(Debug, Parser)]
#[command(name = "harness-evals", version, about)]
struct Cli {
    #[command(subcommand)]
    eval: Eval,
}

#[derive(Debug, Subcommand)]
enum Eval {
    /// M1: facts planted early must be answered exactly after 2+ compactions.
    Needle {
        #[command(flatten)]
        common: Common,
        /// Summary-only compaction and no recall (the baseline).
        #[arg(long)]
        baseline: bool,
        /// Turns in the run.
        #[arg(long, default_value_t = 200)]
        turns: usize,
        /// Facts planted and asked for.
        #[arg(long, default_value_t = 10)]
        needles: usize,
    },
    /// M2: pinned facts stay byte-identical across compactions.
    Fidelity {
        #[command(flatten)]
        common: Common,
        /// Compactions in the run.
        #[arg(long, default_value_t = 50)]
        compactions: usize,
    },
}

#[derive(Debug, clap::Args)]
struct Common {
    /// The model to test, for example openai/gpt-6-luna.
    #[arg(long, required_unless_present = "offline")]
    model: Option<ModelRef>,
    /// Use the deterministic offline model instead (no key, no cost).
    #[arg(long, conflicts_with = "model")]
    offline: bool,
    /// Seed for the scenario.
    #[arg(long, default_value_t = 1)]
    seed: u64,
    /// Compact at this many input tokens (small, so a 200-turn run compacts
    /// several times).
    #[arg(long, default_value_t = 6_000)]
    compact_at: u64,
    /// Effort for the model (low keeps live runs cheap).
    #[arg(long, default_value = "low", value_parser = parse_effort)]
    effort: Effort,
    /// Where the results row goes.
    #[arg(long, default_value = "evals/results.tsv")]
    results: PathBuf,
}

fn parse_effort(s: &str) -> Result<Effort, String> {
    Effort::ALL
        .into_iter()
        .find(|e| e.name() == s)
        .ok_or_else(|| format!("unknown effort {s:?}: low, medium, high, xhigh or max"))
}

/// The provider and price for `common`, from the user's config and keys.
fn provider(common: &Common) -> anyhow::Result<(Arc<dyn Provider>, ModelRef, Option<ModelPrice>)> {
    let Some(model) = common.model.clone().filter(|_| !common.offline) else {
        let model = "offline/model".parse().context("offline model id")?;
        return Ok((Arc::new(offline::OfflineModel), model, None));
    };
    let paths = Paths::discover()?;
    let cwd = std::env::current_dir()?;
    let loaded = ConfigLoader::new(paths)
        .project_dir(&cwd)
        .env(std::env::vars())
        .load()?;
    let config = loaded.config;
    let provider = build_provider(&model, &config, &SystemSecrets)?;
    let price = config.pricing.get(&model.to_string()).map(|p| ModelPrice {
        input: p.input,
        output: p.output,
        cache_read: p.cache_read.unwrap_or(p.input),
        cache_write: p.cache_write.unwrap_or(p.input),
    });
    Ok((provider, model, price))
}

/// Appends one row to the results file, writing the header first if new.
fn record(path: &PathBuf, row: &[String]) -> anyhow::Result<()> {
    const HEADER: &str = "date\teval\tvariant\tmodel\tseed\tturns\tcompactions\titems\tcorrect\tscore_pct\tcost_usd\tnotes";
    let new = !path.exists();
    if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening {}", path.display()))?;
    if new {
        writeln!(file, "{HEADER}")?;
    }
    writeln!(file, "{}", row.join("\t"))?;
    Ok(())
}

/// A progress counter on a terminal; nothing when stderr is redirected.
fn progress(what: &str, i: usize, n: usize) {
    use std::io::IsTerminal as _;
    if std::io::stderr().is_terminal() {
        let _ = write!(std::io::stderr(), "\r{what} {i}/{n}");
    }
}

fn today() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(0));
    harness_context::civil_date(ms)
}

#[allow(clippy::cast_precision_loss)]
fn pct(part: usize, whole: usize) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 * 100.0 / whole as f64
    }
}

#[allow(clippy::cast_precision_loss)]
fn usd(micros: u64) -> String {
    format!("{:.4}", micros as f64 / 1_000_000.0)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let dir = tempfile::tempdir()?;
    let mut err = std::io::stderr();
    match cli.eval {
        Eval::Needle {
            common,
            baseline,
            turns,
            needles,
        } => {
            let (provider, model, price) = provider(&common)?;
            let setup = Setup {
                model: model.clone(),
                price,
                effort: Some(common.effort),
                compact_at: common.compact_at,
                style: if baseline {
                    CompactionStyle::SummaryOnly
                } else {
                    CompactionStyle::Anchored
                },
            };
            let score = evals::needle(
                provider,
                &setup,
                turns,
                needles,
                common.seed,
                dir.path(),
                |i, n| {
                    progress("turn", i, n);
                },
            )
            .await?;
            let variant = if baseline { "summary-only" } else { "anchored" };
            let accuracy = pct(score.correct, score.needles);
            writeln!(
                err,
                "\nneedle ({variant}, {model}): {}/{} correct ({accuracy:.1}%), {} compactions, ${}",
                score.correct,
                score.needles,
                score.compactions,
                usd(score.cost_micros)
            )?;
            for problem in score.failed_turns.iter().chain(&score.violations) {
                writeln!(err, "  {problem}")?;
            }
            let notes = format!(
                "{} failed turns; {} violations",
                score.failed_turns.len(),
                score.violations.len()
            );
            record(
                &common.results,
                &[
                    today(),
                    "needle".into(),
                    variant.into(),
                    model.to_string(),
                    common.seed.to_string(),
                    score.turns.to_string(),
                    score.compactions.to_string(),
                    score.needles.to_string(),
                    score.correct.to_string(),
                    format!("{accuracy:.1}"),
                    usd(score.cost_micros),
                    notes,
                ],
            )?;
        }
        Eval::Fidelity {
            common,
            compactions,
        } => {
            let (provider, model, price) = provider(&common)?;
            let setup = Setup {
                model: model.clone(),
                price,
                effort: Some(common.effort),
                compact_at: u64::MAX,
                style: CompactionStyle::Anchored,
            };
            let score = evals::fidelity(
                provider,
                &setup,
                compactions,
                common.seed,
                dir.path(),
                |i, n| {
                    progress("compaction", i, n);
                },
            )
            .await?;
            let fidelity = pct(score.verbatim, score.checked);
            writeln!(
                err,
                "\nfidelity ({model}): {}/{} pin checks verbatim ({fidelity:.1}%), {} compactions, {}/{} requirements pinned, ${}",
                score.verbatim,
                score.checked,
                score.compactions,
                score.pinned_requirements,
                score.requirements,
                usd(score.cost_micros)
            )?;
            for problem in &score.problems {
                writeln!(err, "  {problem}")?;
            }
            record(
                &common.results,
                &[
                    today(),
                    "fidelity".into(),
                    "anchored".into(),
                    model.to_string(),
                    common.seed.to_string(),
                    String::new(),
                    score.compactions.to_string(),
                    score.checked.to_string(),
                    score.verbatim.to_string(),
                    format!("{fidelity:.1}"),
                    usd(score.cost_micros),
                    format!(
                        "{}/{} requirements pinned; {} problems",
                        score.pinned_requirements,
                        score.requirements,
                        score.problems.len()
                    ),
                ],
            )?;
        }
    }
    Ok(())
}
