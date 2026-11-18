//! Token and cost accounting from `Usage` events (FR-PRV-06): per turn, per
//! task and per session, and the numbers the status bar shows.

use harness_types::{TaskId, Usage};

/// One model call's usage and cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnUsage {
    /// The task the call belonged to.
    pub task: TaskId,
    /// Tokens.
    pub usage: Usage,
    /// Cost in millionths of a dollar (0 when the model has no price).
    pub cost_micros: u64,
}

/// Usage summed over a set of turns.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Totals {
    /// Model calls.
    pub turns: u64,
    /// Uncached input tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache-read input tokens.
    pub cache_read: u64,
    /// Cache-write input tokens.
    pub cache_write: u64,
    /// Cost in millionths of a dollar.
    pub cost_micros: u64,
}

impl Totals {
    fn add(&mut self, t: &TurnUsage) {
        self.turns += 1;
        self.input += t.usage.input;
        self.output += t.usage.output;
        self.cache_read += t.usage.cache_read;
        self.cache_write += t.usage.cache_write;
        self.cost_micros += t.cost_micros;
    }

    /// Share of input tokens read from the cache (0 when there is no input).
    #[must_use]
    pub fn cache_ratio(&self) -> f64 {
        let total = self.input + self.cache_read + self.cache_write;
        if total == 0 {
            0.0
        } else {
            #[allow(clippy::cast_precision_loss)]
            {
                self.cache_read as f64 / total as f64
            }
        }
    }
}

/// Every turn's usage, in order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Ledger {
    turns: Vec<TurnUsage>,
}

impl Ledger {
    /// Records one `Usage` event.
    pub fn record(&mut self, task: TaskId, usage: Usage, cost_micros: u64) {
        self.turns.push(TurnUsage {
            task,
            usage,
            cost_micros,
        });
    }

    /// Every recorded turn.
    #[must_use]
    pub fn turns(&self) -> &[TurnUsage] {
        &self.turns
    }

    /// The most recent turn.
    #[must_use]
    pub fn last(&self) -> Option<&TurnUsage> {
        self.turns.last()
    }

    /// Totals for the whole session.
    #[must_use]
    pub fn session(&self) -> Totals {
        let mut t = Totals::default();
        self.turns.iter().for_each(|u| t.add(u));
        t
    }

    /// Totals per task, in order of first appearance.
    #[must_use]
    pub fn by_task(&self) -> Vec<(TaskId, Totals)> {
        let mut out: Vec<(TaskId, Totals)> = Vec::new();
        for u in &self.turns {
            match out.iter_mut().find(|(t, _)| *t == u.task) {
                Some((_, totals)) => totals.add(u),
                None => {
                    let mut totals = Totals::default();
                    totals.add(u);
                    out.push((u.task, totals));
                }
            }
        }
        out
    }
}

/// `1234` → `1.2k`, `2_500_000` → `2.5M`.
///
/// # Examples
/// ```
/// assert_eq!(harness_tui::ledger::tokens(950), "950");
/// assert_eq!(harness_tui::ledger::tokens(41_200), "41.2k");
/// ```
#[must_use]
pub fn tokens(n: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let f = n as f64;
    match n {
        0..1_000 => n.to_string(),
        1_000..1_000_000 => format!("{:.1}k", f / 1_000.0),
        _ => format!("{:.1}M", f / 1_000_000.0),
    }
}

/// Millionths of a dollar → `$0.42` (or `$0.0031` for small amounts).
///
/// # Examples
/// ```
/// assert_eq!(harness_tui::ledger::money(420_000), "$0.42");
/// assert_eq!(harness_tui::ledger::money(3_100), "$0.0031");
/// ```
#[must_use]
pub fn money(micros: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let dollars = micros as f64 / 1_000_000.0;
    if micros != 0 && dollars < 0.01 {
        format!("${dollars:.4}")
    } else {
        format!("${dollars:.2}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64, cache_read: u64) -> Usage {
        Usage {
            input,
            output,
            cache_read,
            ..Usage::default()
        }
    }

    #[test]
    fn totals_add_up_per_task_and_session() {
        let (a, b) = (TaskId::new(), TaskId::new());
        let mut l = Ledger::default();
        l.record(a, usage(100, 10, 900), 1_000);
        l.record(a, usage(50, 5, 950), 500);
        l.record(b, usage(10, 1, 0), 0);
        let s = l.session();
        assert_eq!(
            (s.turns, s.input, s.output, s.cache_read, s.cost_micros),
            (3, 160, 16, 1_850, 1_500)
        );
        let per = l.by_task();
        assert_eq!(per.len(), 2);
        assert_eq!((per[0].0, per[0].1.turns), (a, 2));
        assert_eq!(l.last().unwrap().task, b);
    }

    #[test]
    fn cache_ratio_is_reads_over_all_input() {
        let mut l = Ledger::default();
        l.record(TaskId::new(), usage(100, 0, 900), 0);
        assert!((l.session().cache_ratio() - 0.9).abs() < 1e-9);
        assert!(Totals::default().cache_ratio().abs() < 1e-9);
    }

    #[test]
    fn numbers_are_short() {
        assert_eq!(tokens(0), "0");
        assert_eq!(tokens(1_000), "1.0k");
        assert_eq!(tokens(250_000), "250.0k");
        assert_eq!(tokens(2_500_000), "2.5M");
        assert_eq!(money(0), "$0.00");
        assert_eq!(money(1_234_567), "$1.23");
    }
}
