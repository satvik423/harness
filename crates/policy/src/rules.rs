//! `rules.toml`: user and project rules (Tech spec 3.6).
//!
//! ```toml
//! [[rule]]
//! tool = "shell"                 # tool names; `|` separates alternatives, `*` is a wildcard
//! match = "prefix:cargo test"    # optional: prefix: | glob: | regex:
//! decision = "allow"             # allow | ask | deny
//!
//! [[rule]]
//! tool = "edit_file|write_file|apply_patch|read_file"
//! path = "glob:**/.env*"         # optional: glob: | prefix: | regex:
//! decision = "deny"
//! reason = "Secrets files are off limits"
//! ```
//!
//! What `match` is compared with depends on the tool: each simple command for
//! `shell`, the tool's own name for MCP tools (`mcp__server__NAME`), and
//! nothing for other tools (a rule with `match` never matches them).
//! `prefix:` compares whole words, so `prefix:cargo test` matches
//! `cargo test --all` but not `cargo testing`.

use std::fmt;
use std::path::{Path, PathBuf};

use globset::{GlobBuilder, GlobMatcher};
use regex::Regex;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A rule's decision. Combined across rules as deny > ask > allow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Decision {
    /// Run without asking.
    Allow,
    /// Ask the user.
    Ask,
    /// Refuse.
    Deny,
}

impl fmt::Display for Decision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Allow => "allow",
            Self::Ask => "ask",
            Self::Deny => "deny",
        })
    }
}

/// One rule as written in `rules.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RawRule {
    /// Tool name patterns separated by `|`.
    pub tool: String,
    /// What the call's subject must match (`prefix:`, `glob:` or `regex:`).
    #[serde(rename = "match", default, skip_serializing_if = "Option::is_none")]
    pub matches: Option<String>,
    /// What one of the call's paths must match (`glob:`, `prefix:` or `regex:`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// The decision when the rule matches.
    pub decision: Decision,
    /// Explanation shown to the user and the model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

/// The contents of a `rules.toml` file.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesFile {
    /// The rules, in file order.
    #[serde(default)]
    pub rule: Vec<RawRule>,
}

/// Errors from reading rules.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuleError {
    /// The file is not valid TOML or has unknown fields.
    #[error("{origin}: {message}")]
    Syntax {
        /// Where the rules came from.
        origin: String,
        /// The parser's message.
        message: String,
    },
    /// A rule has an invalid pattern.
    #[error("{origin}, rule {index}: {message}")]
    Invalid {
        /// Where the rules came from.
        origin: String,
        /// 1-based rule number in the file.
        index: usize,
        /// What is wrong.
        message: String,
    },
    /// The file exists but cannot be read.
    #[error("cannot read {path}: {message}")]
    Read {
        /// The file.
        path: String,
        /// The I/O error.
        message: String,
    },
}

/// A text pattern from a `match` or `path` field.
#[derive(Debug, Clone)]
pub(crate) enum Pattern {
    /// Whole words (for `match`) or a path prefix (for `path`).
    Prefix(Vec<String>),
    /// A glob (`*` crosses `/` in `match`, `**` spans directories in `path`).
    Glob(GlobMatcher),
    /// A regular expression, matched anywhere unless anchored.
    Regex(Regex),
}

/// Which field a pattern came from (they differ in how `prefix:` works).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Field {
    Match,
    Path,
}

impl Pattern {
    fn parse(raw: &str, field: Field, home: Option<&Path>) -> Result<Self, String> {
        let (kind, body) = raw
            .split_once(':')
            .ok_or_else(|| format!("{raw:?} needs a prefix:, glob: or regex: kind"))?;
        if body.is_empty() {
            return Err(format!("{raw:?} has an empty pattern"));
        }
        let body = match (field, home, body.strip_prefix("~/")) {
            (Field::Path, Some(home), Some(rest)) => format!("{}/{rest}", home.display()),
            _ => body.to_owned(),
        };
        match kind {
            "prefix" => Ok(Self::Prefix(match field {
                Field::Match => body.split_whitespace().map(str::to_owned).collect(),
                Field::Path => vec![body],
            })),
            "glob" => GlobBuilder::new(&body)
                .literal_separator(field == Field::Path)
                .build()
                .map(|g| Self::Glob(g.compile_matcher()))
                .map_err(|e| format!("bad glob {body:?}: {e}")),
            "regex" => Regex::new(&body)
                .map(Self::Regex)
                .map_err(|e| format!("bad regex {body:?}: {e}")),
            other => Err(format!(
                "unknown pattern kind {other:?}; use prefix:, glob: or regex:"
            )),
        }
    }

    /// Matches a command's words (`prefix:`) or text (`glob:`, `regex:`).
    pub(crate) fn matches_subject(&self, words: &[Option<String>], text: &str) -> bool {
        match self {
            Self::Prefix(want) => {
                want.len() <= words.len()
                    && want
                        .iter()
                        .zip(words)
                        .all(|(w, got)| got.as_deref() == Some(w.as_str()))
            }
            Self::Glob(g) => g.is_match(text),
            Self::Regex(r) => r.is_match(text),
        }
    }

    /// Matches one path, given both absolute and (when inside `cwd`) relative.
    pub(crate) fn matches_path(&self, path: &Path, cwd: &Path) -> bool {
        let relative = path.strip_prefix(cwd).ok();
        match self {
            Self::Prefix(want) => {
                let want = Path::new(&want[0]);
                path.starts_with(want) || relative.is_some_and(|r| r.starts_with(want))
            }
            Self::Glob(g) => g.is_match(path) || relative.is_some_and(|r| g.is_match(r)),
            Self::Regex(r) => {
                r.is_match(&path.to_string_lossy())
                    || relative.is_some_and(|rel| r.is_match(&rel.to_string_lossy()))
            }
        }
    }
}

/// Where a rule came from, for messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Origin {
    /// Built into the harness (`policy.default_deny_paths`).
    BuiltIn,
    /// A rules file.
    File(PathBuf),
    /// Approved by the user during this session.
    Session,
}

impl fmt::Display for Origin {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BuiltIn => f.write_str("built-in"),
            Self::File(p) => write!(f, "{}", p.display()),
            Self::Session => f.write_str("this session"),
        }
    }
}

/// A validated rule, ready to match.
#[derive(Debug, Clone)]
pub(crate) struct Rule {
    pub(crate) raw: RawRule,
    pub(crate) origin: Origin,
    tools: Vec<GlobMatcher>,
    subject: Option<Pattern>,
    path: Option<Pattern>,
}

impl Rule {
    /// Validates `raw`.
    ///
    /// # Errors
    /// A message naming the bad field.
    pub(crate) fn new(raw: RawRule, origin: Origin, home: Option<&Path>) -> Result<Self, String> {
        let tools = raw
            .tool
            .split('|')
            .map(str::trim)
            .map(|t| {
                if t.is_empty() {
                    return Err("tool has an empty alternative".to_owned());
                }
                GlobBuilder::new(t)
                    .build()
                    .map(|g| g.compile_matcher())
                    .map_err(|e| format!("bad tool pattern {t:?}: {e}"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let subject = raw
            .matches
            .as_deref()
            .map(|m| Pattern::parse(m, Field::Match, home))
            .transpose()
            .map_err(|e| format!("match: {e}"))?;
        let path = raw
            .path
            .as_deref()
            .map(|p| Pattern::parse(p, Field::Path, home))
            .transpose()
            .map_err(|e| format!("path: {e}"))?;
        Ok(Self {
            raw,
            origin,
            tools,
            subject,
            path,
        })
    }

    pub(crate) fn applies_to_tool(&self, tool: &str) -> bool {
        self.tools.iter().any(|t| t.is_match(tool))
    }

    /// Whether the rule matches a unit of a call: an optional subject (words
    /// and text) and the paths it touches.
    pub(crate) fn matches(
        &self,
        subject: Option<(&[Option<String>], &str)>,
        paths: &[PathBuf],
        cwd: &Path,
    ) -> bool {
        let subject_ok = match (&self.subject, subject) {
            (None, _) => true,
            (Some(p), Some((words, text))) => p.matches_subject(words, text),
            (Some(_), None) => false,
        };
        let path_ok = match &self.path {
            None => true,
            Some(p) => paths.iter().any(|path| p.matches_path(path, cwd)),
        };
        subject_ok && path_ok
    }

    /// `rule in <origin>: <reason or pattern>`, for messages.
    pub(crate) fn describe(&self) -> String {
        let what = self.raw.reason.clone().unwrap_or_else(|| {
            let mut parts = vec![format!("tool = {:?}", self.raw.tool)];
            if let Some(m) = &self.raw.matches {
                parts.push(format!("match = {m:?}"));
            }
            if let Some(p) = &self.raw.path {
                parts.push(format!("path = {p:?}"));
            }
            parts.join(", ")
        });
        format!("{} rule ({}): {what}", self.raw.decision, self.origin)
    }
}

/// Parses rules from TOML text.
///
/// # Errors
/// [`RuleError::Syntax`] for bad TOML or unknown fields, [`RuleError::Invalid`]
/// for a bad pattern.
pub(crate) fn parse_rules(
    text: &str,
    origin: &Origin,
    home: Option<&Path>,
) -> Result<Vec<Rule>, RuleError> {
    let file: RulesFile = toml::from_str(text).map_err(|e| RuleError::Syntax {
        origin: origin.to_string(),
        message: e.to_string(),
    })?;
    file.rule
        .into_iter()
        .enumerate()
        .map(|(i, raw)| {
            Rule::new(raw, origin.clone(), home).map_err(|message| RuleError::Invalid {
                origin: origin.to_string(),
                index: i + 1,
                message,
            })
        })
        .collect()
}

/// Reads a rules file; a missing file means no rules.
///
/// # Errors
/// [`RuleError::Read`] if the file exists but cannot be read, or the errors of
/// [`parse_rules`].
pub(crate) fn load_rules(path: &Path, home: Option<&Path>) -> Result<Vec<Rule>, RuleError> {
    match std::fs::read_to_string(path) {
        Ok(text) => parse_rules(&text, &Origin::File(path.to_path_buf()), home),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(RuleError::Read {
            path: path.display().to_string(),
            message: e.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(text: &str) -> Vec<Rule> {
        parse_rules(text, &Origin::BuiltIn, Some(Path::new("/home/u"))).unwrap()
    }

    fn one(text: &str) -> Rule {
        rules(text).remove(0)
    }

    fn lit(words: &[&str]) -> Vec<Option<String>> {
        words.iter().map(|w| Some((*w).to_owned())).collect()
    }

    #[test]
    fn parses_the_spec_example() {
        let r = rules(
            r#"
[[rule]]
tool = "shell"
match = "prefix:cargo test"
decision = "allow"

[[rule]]
tool = "edit_file|write_file|apply_patch|read_file"
path = "glob:**/.env*"
decision = "deny"
reason = "Secrets files are off limits"

[[rule]]
tool = "mcp__github__*"
match = "glob:get_*"
decision = "allow"
"#,
        );
        assert_eq!(r.len(), 3);
        assert_eq!(r[1].raw.decision, Decision::Deny);
        assert!(r[1].applies_to_tool("read_file") && !r[1].applies_to_tool("shell"));
        assert!(r[2].applies_to_tool("mcp__github__get_issue"));
    }

    #[test]
    fn prefix_matches_whole_words() {
        let r =
            one("[[rule]]\ntool = \"shell\"\nmatch = \"prefix:cargo test\"\ndecision = \"allow\"");
        let cwd = Path::new("/p");
        assert!(r.matches(Some((&lit(&["cargo", "test", "--all"]), "")), &[], cwd));
        assert!(r.matches(Some((&lit(&["cargo", "test"]), "")), &[], cwd));
        assert!(!r.matches(Some((&lit(&["cargo", "testing"]), "")), &[], cwd));
        assert!(!r.matches(Some((&lit(&["cargo"]), "")), &[], cwd));
        // A run-time word never matches a literal.
        assert!(!r.matches(Some((&[Some("cargo".into()), None], "")), &[], cwd));
        // No subject (a non-shell tool): a rule with `match` does not apply.
        assert!(!r.matches(None, &[], cwd));
    }

    #[test]
    fn glob_and_regex_match_the_text() {
        let g =
            one("[[rule]]\ntool = \"shell\"\nmatch = \"glob:git * --force*\"\ndecision = \"deny\"");
        assert!(g.matches(Some((&[], "git push origin --force")), &[], Path::new("/")));
        let r = one(
            "[[rule]]\ntool = \"shell\"\nmatch = \"regex:^npm (ci|install)\"\ndecision = \"ask\"",
        );
        assert!(r.matches(Some((&[], "npm ci --silent")), &[], Path::new("/")));
        assert!(!r.matches(Some((&[], "pnpm install")), &[], Path::new("/")));
    }

    #[test]
    fn path_globs_match_absolute_and_relative_paths() {
        let r = one("[[rule]]\ntool = \"*\"\npath = \"glob:**/.env*\"\ndecision = \"deny\"");
        let cwd = Path::new("/proj");
        for p in ["/proj/.env", "/proj/app/.env.local", "/elsewhere/.env"] {
            assert!(r.matches(None, &[PathBuf::from(p)], cwd), "{p}");
        }
        assert!(!r.matches(None, &[PathBuf::from("/proj/src/env.rs")], cwd));
        let top = one("[[rule]]\ntool = \"*\"\npath = \"glob:secrets/*\"\ndecision = \"deny\"");
        assert!(top.matches(None, &[PathBuf::from("/proj/secrets/a")], cwd));
        assert!(!top.matches(None, &[PathBuf::from("/proj/secrets/x/a")], cwd));
    }

    #[test]
    fn tilde_in_paths_means_home() {
        let r = one("[[rule]]\ntool = \"*\"\npath = \"glob:~/.ssh/**\"\ndecision = \"deny\"");
        assert!(r.matches(
            None,
            &[PathBuf::from("/home/u/.ssh/id_ed25519")],
            Path::new("/p")
        ));
        let p = one("[[rule]]\ntool = \"*\"\npath = \"prefix:~/.aws\"\ndecision = \"deny\"");
        assert!(p.matches(
            None,
            &[PathBuf::from("/home/u/.aws/credentials")],
            Path::new("/p")
        ));
        assert!(!p.matches(None, &[PathBuf::from("/home/u/.awsome")], Path::new("/p")));
    }

    #[test]
    fn a_rule_needs_both_its_subject_and_its_path() {
        let r = one(
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:cat\"\npath = \"glob:**/*.pem\"\ndecision = \"deny\"",
        );
        let cwd = Path::new("/p");
        assert!(r.matches(
            Some((&lit(&["cat"]), "")),
            &[PathBuf::from("/p/k.pem")],
            cwd
        ));
        assert!(!r.matches(
            Some((&lit(&["cat"]), "")),
            &[PathBuf::from("/p/k.txt")],
            cwd
        ));
        assert!(!r.matches(
            Some((&lit(&["less"]), "")),
            &[PathBuf::from("/p/k.pem")],
            cwd
        ));
    }

    #[test]
    fn bad_rules_name_the_file_and_rule_number() {
        let origin = Origin::File("/u/rules.toml".into());
        let cases = [
            (
                "[[rule]]\ntool = \"shell\"\ndecision = \"maybe\"",
                "rules.toml",
            ),
            (
                "[[rule]]\ntool = \"shell\"\ndecision = \"allow\"\ncolour = 1",
                "colour",
            ),
            (
                "[[rule]]\ntool = \"x\"\ndecision = \"allow\"\n[[rule]]\ntool = \"shell\"\nmatch = \"cargo\"\ndecision = \"allow\"",
                "rule 2",
            ),
            (
                "[[rule]]\ntool = \"shell\"\nmatch = \"regex:(\"\ndecision = \"allow\"",
                "bad regex",
            ),
            (
                "[[rule]]\ntool = \"shell\"\nmatch = \"fuzzy:x\"\ndecision = \"allow\"",
                "unknown pattern kind",
            ),
            (
                "[[rule]]\ntool = \"a||b\"\ndecision = \"allow\"",
                "empty alternative",
            ),
            (
                "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:\"\ndecision = \"allow\"",
                "empty pattern",
            ),
        ];
        for (text, needle) in cases {
            let e = parse_rules(text, &origin, None).unwrap_err().to_string();
            assert!(e.contains(needle), "{text:?}: {e}");
        }
    }

    #[test]
    fn missing_file_is_no_rules() {
        let dir = tempfile::tempdir().unwrap();
        assert!(
            load_rules(&dir.path().join("none.toml"), None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn describe_prefers_the_reason() {
        let r = one(
            "[[rule]]\ntool = \"read_file\"\npath = \"glob:**/.env*\"\ndecision = \"deny\"\nreason = \"Secrets\"",
        );
        assert_eq!(r.describe(), "deny rule (built-in): Secrets");
        let r = one("[[rule]]\ntool = \"shell\"\nmatch = \"prefix:ls\"\ndecision = \"allow\"");
        assert_eq!(
            r.describe(),
            "allow rule (built-in): tool = \"shell\", match = \"prefix:ls\""
        );
    }

    #[test]
    fn decisions_order_deny_over_ask_over_allow() {
        assert!(Decision::Deny > Decision::Ask && Decision::Ask > Decision::Allow);
    }
}
