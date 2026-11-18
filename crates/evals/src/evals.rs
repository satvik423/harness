//! The two evals of Phase 5 (Tech spec 3.18, M1 and M2).
//!
//! - **needle (M1):** plant facts early, run filler turns that force two or
//!   more compactions, then ask for each fact; a fact counts when the answer
//!   contains it exactly. Run once anchored and once with `--baseline`
//!   (summary-only compaction, no recall); M1 asks for at least 95% and
//!   clearly above the baseline.
//! - **fidelity (M2):** give a new requirement, compact, repeat; after every
//!   compaction each active pin must be an exact quote of its source event
//!   and every earlier pin must be carried over byte for byte. M2 asks for 100%.

use std::path::Path;
use std::sync::Arc;

use anyhow::bail;
use harness_ports::Provider;
use harness_testkit::invariants::check_events;

use crate::driver::{Session, Setup};
use crate::scenario::{Rng, Step, needle_steps, requirement};

/// What a needle run measured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct NeedleScore {
    /// Turns sent.
    pub(crate) turns: usize,
    /// Compactions that happened.
    pub(crate) compactions: u32,
    /// Facts asked for.
    pub(crate) needles: usize,
    /// Facts answered exactly.
    pub(crate) correct: usize,
    /// Cost in millionths of a dollar.
    pub(crate) cost_micros: u64,
    /// Turns that did not complete, with their state.
    pub(crate) failed_turns: Vec<String>,
    /// Broken invariants (I4–I6) in the session's log.
    pub(crate) violations: Vec<String>,
}

/// Runs the needle eval.
pub(crate) async fn needle(
    provider: Arc<dyn Provider>,
    setup: &Setup,
    turns: usize,
    needles: usize,
    seed: u64,
    dir: &Path,
    mut progress: impl FnMut(usize, usize),
) -> anyhow::Result<NeedleScore> {
    let steps = needle_steps(turns, needles, seed);
    let mut session = Session::open(provider, setup, dir).await?;
    let mut score = NeedleScore {
        turns: steps.len(),
        ..NeedleScore::default()
    };
    for (i, step) in steps.iter().enumerate() {
        progress(i + 1, steps.len());
        let turn = session.ask(&step.text()).await?;
        if turn.state != Some(harness_protocol::TaskState::Completed) {
            score
                .failed_turns
                .push(format!("turn {}: {:?}", i + 1, turn.state));
        }
        if let Step::Ask { code, .. } = step {
            score.needles += 1;
            if turn.reply.contains(code.as_str()) {
                score.correct += 1;
            }
        }
    }
    score.compactions = session.compactions;
    score.cost_micros = session.cost_micros;
    score.violations = check_events(&session.events().await?)
        .iter()
        .map(ToString::to_string)
        .collect();
    session.close().await;
    Ok(score)
}

/// What a fidelity run measured.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct FidelityScore {
    /// Compactions that happened.
    pub(crate) compactions: u32,
    /// Pin checks made (every active pin, after every compaction).
    pub(crate) checked: usize,
    /// Checks passed: an exact quote of its event, carried over unchanged,
    /// present verbatim in the opener.
    pub(crate) verbatim: usize,
    /// Requirements the model pinned at least once, of those given.
    pub(crate) pinned_requirements: usize,
    /// Requirements given.
    pub(crate) requirements: usize,
    /// Cost in millionths of a dollar.
    pub(crate) cost_micros: u64,
    /// What failed, for the report.
    pub(crate) problems: Vec<String>,
}

/// Runs the fidelity eval: `compactions` rounds of one requirement and one
/// compaction.
pub(crate) async fn fidelity(
    provider: Arc<dyn Provider>,
    setup: &Setup,
    compactions: usize,
    seed: u64,
    dir: &Path,
    mut progress: impl FnMut(usize, usize),
) -> anyhow::Result<FidelityScore> {
    let mut rng = Rng::new(seed);
    let mut session = Session::open(provider, setup, dir).await?;
    let mut score = FidelityScore::default();
    let mut quotes = Vec::new();
    for k in 1..=compactions {
        progress(k, compactions);
        let (message, quote) = requirement(k, &mut rng);
        quotes.push(quote);
        session.ask(&message).await?;
        let before = session.compacted.len();
        session.compact().await?;
        if session.compacted.len() == before {
            score.problems.push(format!("round {k}: no compaction"));
        }
    }
    score.requirements = quotes.len();
    score.compactions = session.compactions;
    score.cost_micros = session.cost_micros;

    let mut previous: Vec<harness_protocol::PinnedFact> = Vec::new();
    for (i, (record, opener)) in session.compacted.iter().enumerate() {
        for earlier in previous.iter().filter(|p| p.superseded_by.is_none()) {
            score.checked += 1;
            match record.pins.iter().find(|p| p.id == earlier.id) {
                Some(now) if now.quote == earlier.quote && now.seq == earlier.seq => {
                    score.verbatim += 1;
                }
                _ => score.problems.push(format!(
                    "compaction {}: pin p{} was not carried over unchanged",
                    i + 1,
                    earlier.id
                )),
            }
        }
        for pin in record.pins.iter().filter(|p| p.superseded_by.is_none()) {
            score.checked += 1;
            if opener.contains(&pin.quote) {
                score.verbatim += 1;
            } else {
                score.problems.push(format!(
                    "compaction {}: pin p{} is missing from the opener",
                    i + 1,
                    pin.id
                ));
            }
        }
        previous.clone_from(&record.pins);
    }
    // I5 over the whole log: every active pin is an exact quote of its event.
    for violation in check_events(&session.events().await?) {
        score.problems.push(violation.to_string());
    }
    score.pinned_requirements = quotes
        .iter()
        .filter(|q| previous.iter().any(|p| p.quote.contains(q.as_str())))
        .count();
    session.close().await;
    if score.compactions == 0 {
        bail!("no compaction happened; check the model and its key");
    }
    Ok(score)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::offline::OfflineModel;
    use harness_context::compaction::CompactionStyle;

    fn setup(style: CompactionStyle) -> Setup {
        Setup {
            model: "offline/model".parse().unwrap(),
            price: None,
            effort: None,
            compact_at: 6_000,
            style,
        }
    }

    #[tokio::test]
    async fn offline_the_anchored_engine_keeps_every_needle_and_the_baseline_loses_them() {
        let dir = tempfile::tempdir().unwrap();
        let anchored = needle(
            Arc::new(OfflineModel),
            &setup(CompactionStyle::Anchored),
            200,
            10,
            1,
            dir.path(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert!(anchored.compactions >= 2, "{anchored:?}");
        assert_eq!(anchored.correct, anchored.needles, "{anchored:?}");
        assert!(anchored.violations.is_empty(), "{anchored:?}");
        assert!(anchored.failed_turns.is_empty(), "{anchored:?}");

        let dir = tempfile::tempdir().unwrap();
        let baseline = needle(
            Arc::new(OfflineModel),
            &setup(CompactionStyle::SummaryOnly),
            200,
            10,
            1,
            dir.path(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert!(baseline.compactions >= 2);
        assert!(baseline.correct < anchored.correct, "{baseline:?}");
    }

    #[tokio::test]
    async fn offline_fidelity_is_complete_over_fifty_compactions() {
        let dir = tempfile::tempdir().unwrap();
        let score = fidelity(
            Arc::new(OfflineModel),
            &setup(CompactionStyle::Anchored),
            50,
            1,
            dir.path(),
            |_, _| {},
        )
        .await
        .unwrap();
        assert_eq!(score.compactions, 50);
        assert!(score.problems.is_empty(), "{:?}", score.problems);
        assert_eq!(score.checked, score.verbatim);
        assert_eq!(score.pinned_requirements, 50);
    }
}
