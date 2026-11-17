//! Parsing what the user types at approval and question prompts.

use harness_protocol::{Decision, RuleScope};

/// The approval prompt's choices, shown after the action.
pub const APPROVAL_CHOICES: &str =
    "[y]es once / [s]ession / [a]lways / [p]roject always / [n]o (n <why>)";

/// Turns an answer to the approval prompt into a decision.
///
/// `y`/`yes` approves once, `s`/`session` for the session, `a`/`always`
/// permanently for this user, `p`/`project` permanently for this project.
/// Anything else denies; text after `n`/`no` (or any other text) becomes the
/// reason the model sees.
///
/// # Examples
/// ```
/// use harness_cli::prompt::parse_decision;
/// use harness_protocol::Decision;
/// assert_eq!(parse_decision("y"), Decision::Once);
/// assert_eq!(
///     parse_decision("n use cargo check instead"),
///     Decision::Deny { reason: Some("use cargo check instead".into()) }
/// );
/// ```
#[must_use]
pub fn parse_decision(answer: &str) -> Decision {
    let answer = answer.trim();
    let (word, rest) = answer
        .split_once(|c: char| c.is_whitespace() || c == ':' || c == ',')
        .map_or((answer, ""), |(w, r)| (w, r.trim()));
    match word.to_ascii_lowercase().as_str() {
        "y" | "yes" if rest.is_empty() => Decision::Once,
        "s" | "session" if rest.is_empty() => Decision::ForSession,
        "a" | "always" if rest.is_empty() => Decision::Always {
            scope: RuleScope::User,
        },
        "p" | "project" if rest.is_empty() => Decision::Always {
            scope: RuleScope::Project,
        },
        "" => Decision::Deny { reason: None },
        "n" | "no" => Decision::Deny {
            reason: (!rest.is_empty()).then(|| rest.to_owned()),
        },
        // Free text is a denial with that text as guidance for the model.
        _ => Decision::Deny {
            reason: Some(answer.to_owned()),
        },
    }
}

/// Turns an answer to an `ask_user` question into the text the model receives.
///
/// With options, a number picks that option (`2`), and for multi-choice
/// questions several numbers pick several (`1,3`). Anything else is passed on
/// as the user's own words.
///
/// # Examples
/// ```
/// use harness_cli::prompt::parse_answer;
/// let options = vec!["SQLite".to_string(), "Postgres".to_string()];
/// assert_eq!(parse_answer("2", &options, false), "Postgres");
/// assert_eq!(parse_answer("whatever is simplest", &options, false), "whatever is simplest");
/// ```
#[must_use]
pub fn parse_answer(answer: &str, options: &[String], multi: bool) -> String {
    let answer = answer.trim();
    let picks: Option<Vec<&String>> = answer
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|s| !s.is_empty())
        .map(|n| {
            n.parse::<usize>()
                .ok()
                .and_then(|i| i.checked_sub(1))
                .and_then(|i| options.get(i))
        })
        .collect();
    match picks {
        Some(picks) if !picks.is_empty() && (multi || picks.len() == 1) => picks
            .into_iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join(", "),
        _ => answer.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_answers_map_to_decisions() {
        let cases = [
            ("y", Decision::Once),
            ("YES", Decision::Once),
            (" s ", Decision::ForSession),
            ("session", Decision::ForSession),
            (
                "a",
                Decision::Always {
                    scope: RuleScope::User,
                },
            ),
            (
                "always",
                Decision::Always {
                    scope: RuleScope::User,
                },
            ),
            (
                "p",
                Decision::Always {
                    scope: RuleScope::Project,
                },
            ),
            ("", Decision::Deny { reason: None }),
            ("n", Decision::Deny { reason: None }),
            ("no", Decision::Deny { reason: None }),
            (
                "n: tests are slow",
                Decision::Deny {
                    reason: Some("tests are slow".into()),
                },
            ),
            (
                "no, use make",
                Decision::Deny {
                    reason: Some("use make".into()),
                },
            ),
            (
                "try the other file",
                Decision::Deny {
                    reason: Some("try the other file".into()),
                },
            ),
            // "yes but…" is not a clean yes: deny and pass the words on.
            (
                "yes but only src",
                Decision::Deny {
                    reason: Some("yes but only src".into()),
                },
            ),
        ];
        for (answer, want) in cases {
            assert_eq!(parse_decision(answer), want, "{answer:?}");
        }
    }

    #[test]
    fn question_answers_pick_options_or_pass_text() {
        let opts: Vec<String> = ["red", "green", "blue"].map(String::from).to_vec();
        assert_eq!(parse_answer("3", &opts, false), "blue");
        assert_eq!(parse_answer(" 1 ", &opts, false), "red");
        assert_eq!(parse_answer("1,3", &opts, true), "red, blue");
        assert_eq!(parse_answer("1 2", &opts, true), "red, green");
        // Several numbers on a single-choice question, or out of range: keep the text.
        assert_eq!(parse_answer("1,3", &opts, false), "1,3");
        assert_eq!(parse_answer("4", &opts, false), "4");
        assert_eq!(parse_answer("0", &opts, false), "0");
        assert_eq!(parse_answer("purple", &opts, false), "purple");
        assert_eq!(parse_answer("2", &[], false), "2");
    }
}
