//! Deterministic scenarios: the facts to plant, the filler between them and
//! the questions at the end. Seeded, so a run can be repeated exactly.

/// A tiny xorshift generator: enough for picking names and paragraphs, and no
/// dependency.
#[derive(Debug, Clone)]
pub(crate) struct Rng(u64);

impl Rng {
    /// A generator for `seed` (0 is replaced, xorshift cannot start there).
    pub(crate) fn new(seed: u64) -> Self {
        Self(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }

    /// The next number.
    pub(crate) fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    /// A number below `n` (`n` > 0).
    pub(crate) fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n as u64).unwrap_or(0)
    }
}

const SERVICES: &[&str] = &[
    "Falcon", "Harbor", "Juniper", "Lantern", "Meridian", "Nimbus", "Orchid", "Pioneer", "Quartz",
    "Redwood", "Sapphire", "Tundra", "Umber", "Vertex", "Willow", "Xenon", "Yarrow", "Zephyr",
    "Alder", "Beacon", "Cobalt", "Delta", "Ember", "Fjord", "Garnet", "Helix", "Indigo", "Jasper",
];

const THINGS: &[&str] = &[
    "billing worker",
    "search index",
    "session cache",
    "upload queue",
    "audit log",
    "rate limiter",
    "report job",
    "auth gateway",
    "image resizer",
    "mail relay",
];

const VALUES: &[&str] = &[
    "Postgres 14",
    "port 8443",
    "UTC timestamps",
    "gzip level 6",
    "a 30 second timeout",
    "TLS 1.3",
    "UUIDv7 keys",
    "ISO 8601 dates",
    "two replicas",
    "a 512 MB limit",
];

/// Filler prose: realistic but forgettable, so the context grows.
const PARAGRAPHS: &[&str] = &[
    "The team reviewed the deployment checklist and agreed that the staging environment should mirror production more closely. Several services still read configuration from local files, which makes rollbacks slower than they need to be. A shared configuration service was proposed, but nobody wanted to own it yet, so the discussion moved on to monitoring.",
    "Monitoring dashboards show latency spikes every evening between six and seven. The spikes line up with the nightly export job, which scans several large tables without an index. Moving the export to a read replica would help, but the replica lags by a few minutes, and some reports need fresh data.",
    "The mobile client retries failed requests three times with exponential backoff. During the last incident this amplified the load on the gateway, because thousands of clients retried at nearly the same moment. Adding jitter to the backoff was suggested, together with a server-side hint telling clients how long to wait.",
    "Documentation for the internal API is spread across a wiki, a few README files and comments in the code. New engineers keep asking the same questions in chat. One proposal is to generate reference pages from the schema and keep only guides in the wiki, so that the reference never drifts from the code.",
    "The data team wants event timestamps in a single format. Today some services send seconds, some send milliseconds, and one legacy service sends local time without an offset. Converting at ingestion is possible, but it hides the problem, so the team prefers fixing the producers one by one.",
    "Build times grew from four minutes to eleven over the last quarter. Most of the time goes to compiling dependencies that rarely change. Caching them between runs and splitting the slowest test suite into parallel shards could bring the build back under five minutes without changing any code.",
    "Customer support asked for a way to see which feature flags were active for a user at a given time. Flags are evaluated in several services, and only some of them log the result. A small library that records every evaluation with a timestamp would answer the question and make debugging easier.",
    "The storage bill rose because old backups were never deleted. A retention policy of ninety days for daily backups and one year for monthly ones was drafted. Legal still needs to confirm that no contract requires longer retention for some customers before the policy is enforced.",
];

/// One step of a needle run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Step {
    /// Plant a fact (the answer to a later question).
    Plant { service: String, code: String },
    /// Filler that grows the context.
    Filler(String),
    /// Ask for a planted fact; `code` is the expected answer.
    Ask { service: String, code: String },
}

impl Step {
    /// The user message for this step.
    pub(crate) fn text(&self) -> String {
        match self {
            Self::Plant { service, code } => format!(
                "Note for later, no action needed: the deploy code for service {service} is {code}. Reply with OK."
            ),
            Self::Filler(paragraph) => {
                format!("Summarise this in one short sentence:\n\n{paragraph}")
            }
            Self::Ask { service, .. } => {
                format!("What is the deploy code for service {service}? Reply with the code only.")
            }
        }
    }
}

/// The steps of a needle run: `needles` facts planted from the second turn,
/// filler, then one question per fact in the last turns. `turns` is at least
/// `2 × needles + 1`.
pub(crate) fn needle_steps(turns: usize, needles: usize, seed: u64) -> Vec<Step> {
    let mut rng = Rng::new(seed);
    let needles = needles.min(SERVICES.len());
    let turns = turns.max(2 * needles + 1);
    let mut names: Vec<&str> = SERVICES.to_vec();
    let facts: Vec<(String, String)> = (0..needles)
        .map(|_| {
            let name = names.remove(rng.below(names.len()));
            let code = format!(
                "{:04}-{}{}",
                rng.below(10_000),
                char::from(b'A' + u8::try_from(rng.below(26)).unwrap_or(0)),
                char::from(b'A' + u8::try_from(rng.below(26)).unwrap_or(0))
            );
            (name.to_owned(), code)
        })
        .collect();
    let mut steps = vec![Step::Filler(
        PARAGRAPHS[rng.below(PARAGRAPHS.len())].to_owned(),
    )];
    steps.extend(facts.iter().map(|(s, c)| Step::Plant {
        service: s.clone(),
        code: c.clone(),
    }));
    while steps.len() < turns - needles {
        steps.push(Step::Filler(
            PARAGRAPHS[rng.below(PARAGRAPHS.len())].to_owned(),
        ));
    }
    steps.extend(
        facts
            .into_iter()
            .map(|(service, code)| Step::Ask { service, code }),
    );
    steps
}

/// The `k`-th requirement of a fidelity run: (message, the quote to keep).
pub(crate) fn requirement(k: usize, rng: &mut Rng) -> (String, String) {
    let thing = THINGS[rng.below(THINGS.len())];
    let value = VALUES[rng.below(VALUES.len())];
    let fact = format!("the {thing} #{k} must use {value}");
    (
        format!(
            "Requirement {k}: {fact}. Keep this in mind for the rest of the project. {}",
            PARAGRAPHS[rng.below(PARAGRAPHS.len())]
        ),
        fact,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_needle_run_plants_fills_and_asks_in_that_order() {
        let steps = needle_steps(30, 5, 7);
        assert_eq!(steps.len(), 30);
        assert!(matches!(steps[1], Step::Plant { .. }));
        assert!(matches!(steps[24], Step::Filler(_)));
        let asked: Vec<&Step> = steps
            .iter()
            .filter(|s| matches!(s, Step::Ask { .. }))
            .collect();
        assert_eq!(asked.len(), 5);
        assert!(matches!(steps[29], Step::Ask { .. }));
        assert_eq!(needle_steps(30, 5, 7), steps, "seeded runs repeat exactly");
    }

    #[test]
    fn requirements_carry_their_quote() {
        let mut rng = Rng::new(1);
        let (message, quote) = requirement(3, &mut rng);
        assert!(message.contains(&quote));
        assert!(quote.contains("#3 must use"));
    }
}
