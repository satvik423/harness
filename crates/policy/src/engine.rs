//! [`PolicyEngine`]: the [`Policy`] port on rules, profiles and shell analysis.
//!
//! Evaluation (Tech spec 3.6):
//!
//! 1. Split the call into units. A shell command gives one unit per simple
//!    command; any other call is one unit.
//! 2. Match every rule from every layer (built-in, user, trusted project,
//!    session approvals) against each unit. A unit's decision is the strongest
//!    match (deny > ask > allow), or the profile default when nothing matches.
//! 3. The call's decision is the strongest unit decision; constructs the shell
//!    analysis cannot judge force at least ask.
//! 4. Under approval policy `never`, ask becomes deny, with a reason for the model.
//!
//! A rule with a `path` applies when any of the call's paths matches it; an
//! `allow` rule with a `path` needs every path to match, so allowing one file
//! never allows a call that also touches another.

use std::fs::OpenOptions;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};
use std::sync::{PoisonError, RwLock};

use harness_ports::{GrantScope, Policy, PolicyCall, PolicyError, Verdict};
use harness_types::Risk;

use crate::rules::{Decision, Origin, RawRule, Rule, RuleError, RulesFile, load_rules};
use crate::shell::analyze;

/// A named pairing of sandbox mode and approval policy (Tech spec 3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    /// Explore freely; writes and unsandboxed commands need approval.
    ReadOnly,
    /// Edit inside the project; ask to leave it, use the network or escalate.
    Auto,
    /// Like `auto`, but never asks: what would need approval is denied.
    Ci,
    /// No walls and no questions (requires `--dangerously-full-access`).
    Full,
}

impl Profile {
    /// The approval policy that comes with the profile.
    #[must_use]
    pub fn default_approval(self) -> ApprovalPolicy {
        match self {
            Self::ReadOnly | Self::Auto => ApprovalPolicy::OnRequest,
            Self::Ci | Self::Full => ApprovalPolicy::Never,
        }
    }
}

/// When the user is asked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalPolicy {
    /// Ask when a call needs approval.
    OnRequest,
    /// Never ask: a call that would need approval is denied (headless).
    Never,
}

/// Everything the engine needs; built by the front-end from the config.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySettings {
    /// The active profile.
    pub profile: Profile,
    /// The effective approval policy (the profile's, unless overridden).
    pub approval: ApprovalPolicy,
    /// Whether commands run inside an isolating sandbox (Phase 13).
    pub sandboxed: bool,
    /// Globs always denied to every tool (`policy.default_deny_paths`).
    pub default_deny_paths: Vec<String>,
    /// The user's home directory, for `~/` in path patterns.
    pub home: Option<PathBuf>,
    /// The user's rules file (`~/.config/harness/rules.toml`).
    pub user_rules: Option<PathBuf>,
    /// The project's rules file; `None` when the project is not trusted.
    pub project_rules: Option<PathBuf>,
}

/// One unit of a call: what rules are matched against.
struct Unit {
    /// Words and text for `match` patterns (shell part, MCP tool name, host).
    subject: Option<(Vec<Option<String>>, String)>,
    /// Paths for `path` patterns.
    paths: Vec<PathBuf>,
    /// What to ask about when the profile default is to ask.
    action: String,
}

/// The policy engine. Cheap to share: wrap it in an `Arc`.
#[derive(Debug)]
pub struct PolicyEngine {
    settings: PolicySettings,
    builtin: Vec<Rule>,
    user: RwLock<Vec<Rule>>,
    project: RwLock<Vec<Rule>>,
    session: RwLock<Vec<Rule>>,
}

impl PolicyEngine {
    /// Builds the engine and loads the rules files.
    ///
    /// # Errors
    /// [`RuleError`] if a rules file cannot be read or has an invalid rule, or a
    /// default deny path is not a valid glob.
    pub fn new(settings: PolicySettings) -> Result<Self, RuleError> {
        let home = settings.home.as_deref();
        let builtin = settings
            .default_deny_paths
            .iter()
            .enumerate()
            .map(|(i, glob)| {
                let raw = RawRule {
                    tool: "*".into(),
                    matches: None,
                    path: Some(format!("glob:{glob}")),
                    decision: Decision::Deny,
                    reason: Some(format!("{glob} is protected (policy.default_deny_paths)")),
                };
                Rule::new(raw, Origin::BuiltIn, home).map_err(|message| RuleError::Invalid {
                    origin: "policy.default_deny_paths".into(),
                    index: i + 1,
                    message,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let load = |path: &Option<PathBuf>| match path {
            Some(p) => load_rules(p, home),
            None => Ok(Vec::new()),
        };
        Ok(Self {
            user: RwLock::new(load(&settings.user_rules)?),
            project: RwLock::new(load(&settings.project_rules)?),
            session: RwLock::new(Vec::new()),
            builtin,
            settings,
        })
    }

    /// The settings the engine was built with.
    #[must_use]
    pub fn settings(&self) -> &PolicySettings {
        &self.settings
    }

    fn units(call: &PolicyCall, force: &mut Vec<String>) -> Vec<Unit> {
        let cwd = &call.cwd;
        match &call.risk {
            Risk::Exec { command } => {
                let analysis = analyze(command);
                force.extend(analysis.force_ask);
                analysis
                    .parts
                    .into_iter()
                    .map(|part| {
                        let mut paths: Vec<PathBuf> = part
                            .words
                            .iter()
                            .skip(1)
                            .flatten()
                            .filter(|w| !w.starts_with('-') && !w.contains('='))
                            .chain(&part.reads)
                            .map(|w| resolve(cwd, w))
                            .collect();
                        for target in &part.writes {
                            if is_device(target) {
                                continue;
                            }
                            let path = resolve(cwd, target);
                            if !inside(&path, &call.writable_roots) {
                                force.push(format!(
                                    "it writes outside the workspace ({})",
                                    path.display()
                                ));
                            }
                            paths.push(path);
                        }
                        Unit {
                            action: format!("run `{}`", part.text),
                            subject: Some((part.words, part.text)),
                            paths,
                        }
                    })
                    .collect()
            }
            Risk::ReadOnly { paths } | Risk::Write { paths } => {
                let paths: Vec<PathBuf> = paths
                    .iter()
                    .map(|p| normalize(&resolve(cwd, &p.to_string_lossy())))
                    .collect();
                let verb = if call.risk.is_read_only() {
                    "read"
                } else {
                    "write"
                };
                vec![Unit {
                    action: format!("{verb} {}", list(&paths)),
                    subject: mcp_subject(&call.tool),
                    paths,
                }]
            }
            Risk::Network { host } => vec![Unit {
                action: format!("contact {host}"),
                subject: Some((vec![Some(host.clone())], host.clone())),
                paths: Vec::new(),
            }],
            Risk::External { what } => vec![Unit {
                action: what.clone(),
                subject: mcp_subject(&call.tool),
                paths: Vec::new(),
            }],
        }
    }

    /// The profile's decision for a unit no rule matched.
    fn default_for(&self, call: &PolicyCall, unit: &Unit) -> (Decision, String) {
        let s = &self.settings;
        if s.profile == Profile::Full {
            return (Decision::Allow, "full access profile".into());
        }
        match &call.risk {
            Risk::ReadOnly { .. } => (Decision::Allow, "read-only call".into()),
            Risk::Write { .. } => {
                let outside: Vec<&PathBuf> = unit
                    .paths
                    .iter()
                    .filter(|p| !inside(p, &call.writable_roots))
                    .collect();
                if !outside.is_empty() {
                    (
                        Decision::Ask,
                        format!(
                            "{} is outside the workspace",
                            list(&outside.into_iter().cloned().collect::<Vec<_>>())
                        ),
                    )
                } else if s.profile == Profile::ReadOnly {
                    (
                        Decision::Ask,
                        "the read-only profile asks before writes".into(),
                    )
                } else {
                    (Decision::Allow, "write inside the workspace".into())
                }
            }
            Risk::Exec { .. } => {
                if s.sandboxed && s.approval == ApprovalPolicy::OnRequest {
                    (Decision::Allow, "runs in the sandbox".into())
                } else {
                    (Decision::Ask, "commands run without a sandbox".into())
                }
            }
            Risk::Network { .. } => (Decision::Ask, "network access".into()),
            Risk::External { .. } => (Decision::Ask, "starts something outside this task".into()),
        }
    }

    /// Strongest matching rule for a unit, if any.
    fn strongest(&self, tool: &str, unit: &Unit, cwd: &Path) -> Option<(Decision, String)> {
        let user = self.user.read().unwrap_or_else(PoisonError::into_inner);
        let project = self.project.read().unwrap_or_else(PoisonError::into_inner);
        let session = self.session.read().unwrap_or_else(PoisonError::into_inner);
        let subject = unit
            .subject
            .as_ref()
            .map(|(words, text)| (words.as_slice(), text.as_str()));
        self.builtin
            .iter()
            .chain(user.iter())
            .chain(project.iter())
            .chain(session.iter())
            .filter(|r| r.applies_to_tool(tool) && r.matches(subject, &unit.paths, cwd))
            // An allow rule with a path must cover every path of the unit.
            .filter(|r| {
                r.raw.decision != Decision::Allow
                    || r.raw.path.is_none()
                    || unit
                        .paths
                        .iter()
                        .all(|p| r.matches(subject, std::slice::from_ref(p), cwd))
            })
            .map(|r| (r.raw.decision, r.describe()))
            .max_by_key(|(d, _)| *d)
    }

    fn rules_for(call: &PolicyCall) -> Result<Vec<RawRule>, PolicyError> {
        let rule = |matches: Option<String>, path: Option<String>| RawRule {
            tool: call.tool.clone(),
            matches,
            path,
            decision: Decision::Allow,
            reason: None,
        };
        match &call.risk {
            Risk::ReadOnly { .. } => Ok(Vec::new()),
            Risk::Exec { command } => {
                let analysis = analyze(command);
                if !analysis.force_ask.is_empty() {
                    return Err(PolicyError::NotRememberable(analysis.force_ask.join("; ")));
                }
                analysis
                    .parts
                    .iter()
                    .map(|part| {
                        if !part.is_static() {
                            return Err(PolicyError::NotRememberable(format!(
                                "`{}` uses values known only at run time",
                                part.text
                            )));
                        }
                        // Exact match: approving `rm -rf build` must not allow `rm -rf build ~`.
                        Ok(rule(
                            Some(format!("regex:^{}$", regex::escape(&part.text))),
                            None,
                        ))
                    })
                    .collect()
            }
            Risk::Write { paths } => Ok(paths
                .iter()
                .map(|p| {
                    let p = normalize(&resolve(&call.cwd, &p.to_string_lossy()));
                    rule(
                        None,
                        Some(format!("glob:{}", globset::escape(&p.to_string_lossy()))),
                    )
                })
                .collect()),
            Risk::Network { host } => Ok(vec![rule(
                Some(format!("glob:{}", globset::escape(host))),
                None,
            )]),
            Risk::External { .. } => Ok(vec![rule(None, None)]),
        }
    }
}

impl Policy for PolicyEngine {
    fn evaluate(&self, call: &PolicyCall) -> Verdict {
        let mut force = Vec::new();
        let units = Self::units(call, &mut force);
        let mut decision = Decision::Allow;
        let mut why = Vec::new();
        let mut actions = Vec::new();
        for unit in &units {
            let (d, reason) = self
                .strongest(&call.tool, unit, &call.cwd)
                .unwrap_or_else(|| self.default_for(call, unit));
            if d > decision {
                decision = d;
                why.clear();
                actions.clear();
            }
            if d == decision {
                why.push(reason);
                actions.push(unit.action.clone());
            }
        }
        if !force.is_empty() && decision == Decision::Allow {
            decision = Decision::Ask;
            why.clear();
            actions = units.iter().map(|u| u.action.clone()).collect();
        }
        if decision == Decision::Ask {
            why.extend(force);
        }
        why.dedup();
        let action = if actions.is_empty() {
            format!("call {}", call.tool)
        } else {
            actions.join("; ")
        };
        match decision {
            Decision::Allow => Verdict::Allow {
                reason: why.join("; "),
            },
            Decision::Deny => Verdict::Deny {
                reason: format!(
                    "Denied by policy: {}. Do not retry this call; continue without it or explain what you need.",
                    why.join("; ")
                ),
                approval_required: false,
            },
            Decision::Ask if self.settings.approval == ApprovalPolicy::Never => Verdict::Deny {
                reason: format!(
                    "Denied: {action} needs approval ({}), and this session does not ask (approval policy: never). Do not retry; continue without it or explain what you need.",
                    why.join("; ")
                ),
                approval_required: true,
            },
            Decision::Ask => Verdict::Ask {
                reason: format!("{action} ({})", why.join("; ")),
            },
        }
    }

    fn remember(&self, call: &PolicyCall, scope: GrantScope) -> Result<String, PolicyError> {
        let raws = Self::rules_for(call)?;
        if raws.is_empty() {
            return Ok("nothing to remember: the call is already allowed".into());
        }
        let home = self.settings.home.as_deref();
        let (layer, origin) = match scope {
            GrantScope::Session => (&self.session, Origin::Session),
            GrantScope::User => match &self.settings.user_rules {
                Some(p) => (&self.user, Origin::File(p.clone())),
                None => {
                    return Err(PolicyError::NotRememberable(
                        "no user rules file is configured".into(),
                    ));
                }
            },
            GrantScope::Project => match &self.settings.project_rules {
                Some(p) => (&self.project, Origin::File(p.clone())),
                None => {
                    return Err(PolicyError::NotRememberable(
                        "the project is not trusted, so its rules are not used; choose \"always for me\" instead".into(),
                    ));
                }
            },
        };
        let rules = raws
            .iter()
            .map(|raw| Rule::new(raw.clone(), origin.clone(), home))
            .collect::<Result<Vec<_>, _>>()
            .map_err(PolicyError::NotRememberable)?;
        if let Origin::File(path) = &origin {
            append_rules(path, &raws)?;
        }
        let described: Vec<String> = rules.iter().map(Rule::describe).collect();
        layer
            .write()
            .unwrap_or_else(PoisonError::into_inner)
            .extend(rules);
        Ok(described.join("; "))
    }
}

/// Appends rules to a rules file, creating it (and its directory) if needed.
fn append_rules(path: &Path, raws: &[RawRule]) -> Result<(), PolicyError> {
    let save = |message: String| PolicyError::Save {
        path: path.display().to_string(),
        message,
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| save(e.to_string()))?;
    }
    let body = toml::to_string(&RulesFile {
        rule: raws.to_vec(),
    })
    .map_err(|e| save(e.to_string()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(|e| save(e.to_string()))?;
    write!(
        file,
        "\n# Added by harness when you chose \"always\".\n{body}"
    )
    .map_err(|e| save(e.to_string()))
}

/// The MCP tool's own name (`mcp__server__NAME`), the subject of `match` rules.
fn mcp_subject(tool: &str) -> Option<(Vec<Option<String>>, String)> {
    let rest = tool.strip_prefix("mcp__")?;
    let (_, name) = rest.split_once("__")?;
    Some((vec![Some(name.to_owned())], name.to_owned()))
}

fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    normalize(&if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    })
}

/// Lexically removes `.` and resolves `..`, so `/p/../etc` is judged as `/etc`.
fn normalize(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other),
        }
    }
    out
}

fn inside(path: &Path, roots: &[PathBuf]) -> bool {
    let path = normalize(path);
    roots.iter().any(|r| path.starts_with(normalize(r)))
}

fn is_device(target: &str) -> bool {
    matches!(target, "/dev/null" | "/dev/stdout" | "/dev/stderr")
}

fn list(paths: &[PathBuf]) -> String {
    if paths.is_empty() {
        return "files".into();
    }
    paths
        .iter()
        .map(|p| p.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const CWD: &str = "/proj";

    fn settings(profile: Profile) -> PolicySettings {
        PolicySettings {
            profile,
            approval: profile.default_approval(),
            sandboxed: false,
            default_deny_paths: vec![
                "**/.env*".into(),
                "~/.ssh/**".into(),
                "**/.harness/**".into(),
            ],
            home: Some("/home/u".into()),
            user_rules: None,
            project_rules: None,
        }
    }

    fn engine_with(profile: Profile, user_rules: &str) -> (PolicyEngine, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rules.toml");
        std::fs::write(&path, user_rules).unwrap();
        let mut s = settings(profile);
        s.user_rules = Some(path);
        (PolicyEngine::new(s).unwrap(), dir)
    }

    fn engine(profile: Profile) -> PolicyEngine {
        PolicyEngine::new(settings(profile)).unwrap()
    }

    fn call(tool: &str, risk: Risk) -> PolicyCall {
        PolicyCall {
            tool: tool.into(),
            input: json!({}),
            risk,
            cwd: CWD.into(),
            writable_roots: vec![CWD.into()],
        }
    }

    fn shell(cmd: &str) -> PolicyCall {
        call(
            "shell",
            Risk::Exec {
                command: cmd.into(),
            },
        )
    }

    fn read(p: &str) -> PolicyCall {
        call(
            "read_file",
            Risk::ReadOnly {
                paths: vec![p.into()],
            },
        )
    }

    fn write(p: &str) -> PolicyCall {
        call(
            "write_file",
            Risk::Write {
                paths: vec![p.into()],
            },
        )
    }

    fn kind(v: &Verdict) -> &'static str {
        match v {
            Verdict::Allow { .. } => "allow",
            Verdict::Ask { .. } => "ask",
            Verdict::Deny { .. } => "deny",
        }
    }

    #[test]
    fn reads_are_allowed_except_protected_paths() {
        let e = engine(Profile::Auto);
        assert_eq!(kind(&e.evaluate(&read("/proj/src/main.rs"))), "allow");
        assert_eq!(kind(&e.evaluate(&read("/etc/hosts"))), "allow");
        for protected in [
            "/proj/.env",
            "/proj/api/.env.production",
            "/home/u/.ssh/id_rsa",
            "/proj/.harness/state.db",
        ] {
            let v = e.evaluate(&read(protected));
            assert_eq!(kind(&v), "deny", "{protected}");
        }
        let Verdict::Deny {
            reason,
            approval_required: false,
        } = e.evaluate(&read("/proj/.env"))
        else {
            panic!()
        };
        assert!(reason.contains("**/.env* is protected"), "{reason}");
        assert!(reason.contains("Do not retry"), "{reason}");
    }

    #[test]
    fn relative_and_dotted_paths_are_normalised_before_matching() {
        let e = engine(Profile::Auto);
        assert_eq!(kind(&e.evaluate(&read("/proj/src/../.env"))), "deny");
        assert_eq!(
            kind(&e.evaluate(&write("/proj/src/../../etc/passwd"))),
            "ask"
        );
    }

    #[test]
    fn writes_follow_the_profile_and_the_workspace() {
        assert_eq!(
            kind(&engine(Profile::Auto).evaluate(&write("/proj/a.rs"))),
            "allow"
        );
        assert_eq!(
            kind(&engine(Profile::Auto).evaluate(&write("/tmp/a.rs"))),
            "ask"
        );
        assert_eq!(
            kind(&engine(Profile::ReadOnly).evaluate(&write("/proj/a.rs"))),
            "ask"
        );
        assert_eq!(
            kind(&engine(Profile::Full).evaluate(&write("/tmp/a.rs"))),
            "allow"
        );
        let Verdict::Ask { reason } = engine(Profile::Auto).evaluate(&write("/tmp/a.rs")) else {
            panic!()
        };
        assert_eq!(
            reason,
            "write /tmp/a.rs (/tmp/a.rs is outside the workspace)"
        );
    }

    #[test]
    fn unsandboxed_commands_ask_unless_a_rule_allows_them() {
        let (e, _d) = engine_with(
            Profile::Auto,
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:cargo test\"\ndecision = \"allow\"",
        );
        assert_eq!(kind(&e.evaluate(&shell("cargo test --all"))), "allow");
        let Verdict::Ask { reason } = e.evaluate(&shell("cargo build")) else {
            panic!()
        };
        assert_eq!(reason, "run `cargo build` (commands run without a sandbox)");
    }

    #[test]
    fn each_part_of_a_compound_command_is_judged() {
        let (e, _d) = engine_with(
            Profile::Auto,
            r#"
[[rule]]
tool = "shell"
match = "prefix:cargo test"
decision = "allow"
[[rule]]
tool = "shell"
match = "prefix:git push"
decision = "ask"
reason = "pushing is public"
[[rule]]
tool = "shell"
match = "prefix:rm -rf /"
decision = "deny"
"#,
        );
        let Verdict::Ask { reason } = e.evaluate(&shell("cargo test && git push")) else {
            panic!()
        };
        assert_eq!(
            reason,
            "run `git push` (ask rule (".to_owned()
                + &e.settings()
                    .user_rules
                    .as_ref()
                    .unwrap()
                    .display()
                    .to_string()
                + "): pushing is public)"
        );
        assert_eq!(kind(&e.evaluate(&shell("cargo test; rm -rf /"))), "deny");
        assert_eq!(kind(&e.evaluate(&shell("cargo test | tee log"))), "ask");
    }

    #[test]
    fn substitutions_force_ask_even_when_rules_allow() {
        let (e, _d) = engine_with(
            Profile::Auto,
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:echo\"\ndecision = \"allow\"",
        );
        assert_eq!(kind(&e.evaluate(&shell("echo hi"))), "allow");
        let Verdict::Ask { reason } = e.evaluate(&shell("echo $(cat ~/.aws/credentials)")) else {
            panic!()
        };
        assert!(reason.contains("command substitution"), "{reason}");
    }

    #[test]
    fn redirects_outside_the_workspace_force_ask() {
        let (e, _d) = engine_with(
            Profile::Auto,
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:echo\"\ndecision = \"allow\"",
        );
        assert_eq!(kind(&e.evaluate(&shell("echo hi > notes.txt"))), "allow");
        assert_eq!(kind(&e.evaluate(&shell("echo hi > /dev/null"))), "allow");
        let Verdict::Ask { reason } = e.evaluate(&shell("echo hi >> /etc/hosts")) else {
            panic!()
        };
        assert!(
            reason.contains("writes outside the workspace (/etc/hosts)"),
            "{reason}"
        );
    }

    #[test]
    fn shell_arguments_naming_protected_files_are_denied() {
        let e = engine(Profile::Auto);
        assert_eq!(kind(&e.evaluate(&shell("cat .env"))), "deny");
        assert_eq!(kind(&e.evaluate(&shell("cp ~/.ssh/id_rsa /tmp/x"))), "ask");
        assert_eq!(
            kind(&e.evaluate(&shell("cp /home/u/.ssh/id_rsa /tmp/x"))),
            "deny"
        );
    }

    #[test]
    fn sandboxed_commands_run_under_on_request() {
        let mut s = settings(Profile::Auto);
        s.sandboxed = true;
        let e = PolicyEngine::new(s).unwrap();
        assert_eq!(kind(&e.evaluate(&shell("make"))), "allow");
        assert_eq!(kind(&e.evaluate(&shell("make && echo $(id)"))), "ask");
    }

    #[test]
    fn approval_policy_never_turns_ask_into_deny_with_a_reason() {
        let v = engine(Profile::Ci).evaluate(&shell("cargo build"));
        let Verdict::Deny {
            reason,
            approval_required: true,
        } = v
        else {
            panic!("{v:?}")
        };
        assert!(
            reason.starts_with("Denied: run `cargo build` needs approval"),
            "{reason}"
        );
        assert!(reason.contains("approval policy: never"), "{reason}");
        assert_eq!(
            kind(&engine(Profile::Ci).evaluate(&write("/proj/a"))),
            "allow"
        );
    }

    #[test]
    fn full_profile_allows_everything_but_deny_rules() {
        let e = engine(Profile::Full);
        assert_eq!(kind(&e.evaluate(&shell("rm -rf build"))), "allow");
        assert_eq!(
            kind(&e.evaluate(&call(
                "web_fetch",
                Risk::Network {
                    host: "x.io".into()
                }
            ))),
            "allow"
        );
        assert_eq!(kind(&e.evaluate(&read("/proj/.env"))), "deny");
    }

    #[test]
    fn network_and_external_ask_and_mcp_names_are_subjects() {
        let (e, _d) = engine_with(
            Profile::Auto,
            "[[rule]]\ntool = \"mcp__github__*\"\nmatch = \"glob:get_*\"\ndecision = \"allow\"",
        );
        assert_eq!(
            kind(&e.evaluate(&call(
                "web_fetch",
                Risk::Network {
                    host: "docs.rs".into()
                }
            ))),
            "ask"
        );
        let mcp = |name: &str| {
            call(
                name,
                Risk::External {
                    what: format!("call {name}"),
                },
            )
        };
        assert_eq!(kind(&e.evaluate(&mcp("mcp__github__get_issue"))), "allow");
        assert_eq!(kind(&e.evaluate(&mcp("mcp__github__create_issue"))), "ask");
    }

    #[test]
    fn an_allow_rule_with_a_path_must_cover_every_path() {
        let (e, _d) = engine_with(
            Profile::ReadOnly,
            "[[rule]]\ntool = \"apply_patch\"\npath = \"glob:docs/**\"\ndecision = \"allow\"",
        );
        let patch = |paths: &[&str]| {
            call(
                "apply_patch",
                Risk::Write {
                    paths: paths.iter().map(PathBuf::from).collect(),
                },
            )
        };
        assert_eq!(kind(&e.evaluate(&patch(&["/proj/docs/a.md"]))), "allow");
        assert_eq!(
            kind(&e.evaluate(&patch(&["/proj/docs/a.md", "/proj/src/b.rs"]))),
            "ask"
        );
    }

    #[test]
    fn session_grants_allow_the_exact_command_only() {
        let e = engine(Profile::Auto);
        let c = shell("npm test -- --watch=false");
        assert_eq!(kind(&e.evaluate(&c)), "ask");
        let described = e.remember(&c, GrantScope::Session).unwrap();
        assert!(described.contains("this session"), "{described}");
        assert_eq!(kind(&e.evaluate(&c)), "allow");
        assert_eq!(
            kind(&e.evaluate(&shell("npm test -- --watch=false; rm x"))),
            "ask"
        );
        assert_eq!(
            kind(&e.evaluate(&shell("npm test -- --watch=false extra"))),
            "ask"
        );
    }

    #[test]
    fn grants_never_override_ask_rules() {
        let (e, _d) = engine_with(
            Profile::Auto,
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:git push\"\ndecision = \"ask\"",
        );
        let c = shell("git push");
        e.remember(&c, GrantScope::Session).unwrap();
        assert_eq!(kind(&e.evaluate(&c)), "ask");
    }

    #[test]
    fn commands_with_substitutions_or_variables_cannot_be_remembered() {
        let e = engine(Profile::Auto);
        for cmd in ["echo $(id)", "rm -rf $DIR"] {
            assert!(
                matches!(
                    e.remember(&shell(cmd), GrantScope::Session),
                    Err(PolicyError::NotRememberable(_))
                ),
                "{cmd}"
            );
        }
    }

    #[test]
    fn always_writes_a_rule_that_a_new_engine_loads() {
        let (e, dir) = engine_with(Profile::Auto, "");
        let c = shell("cargo clippy --all-targets");
        e.remember(&c, GrantScope::User).unwrap();
        let text = std::fs::read_to_string(dir.path().join("rules.toml")).unwrap();
        assert!(text.contains("Added by harness"), "{text}");
        let mut s = settings(Profile::Auto);
        s.user_rules = Some(dir.path().join("rules.toml"));
        let fresh = PolicyEngine::new(s).unwrap();
        assert_eq!(kind(&fresh.evaluate(&c)), "allow");
        assert_eq!(kind(&fresh.evaluate(&shell("cargo clippy"))), "ask");
    }

    #[test]
    fn remembered_writes_cover_that_path_only() {
        let (e, _d) = engine_with(Profile::Auto, "");
        let outside = write("/tmp/out [1].txt");
        e.remember(&outside, GrantScope::Session).unwrap();
        assert_eq!(kind(&e.evaluate(&outside)), "allow");
        assert_eq!(kind(&e.evaluate(&write("/tmp/out 1.txt"))), "ask");
    }

    #[test]
    fn project_grants_need_a_trusted_project() {
        let e = engine(Profile::Auto);
        let err = e.remember(&shell("make"), GrantScope::Project).unwrap_err();
        assert!(err.to_string().contains("not trusted"), "{err}");
        let dir = tempfile::tempdir().unwrap();
        let mut s = settings(Profile::Auto);
        s.project_rules = Some(dir.path().join(".harness/rules.toml"));
        let e = PolicyEngine::new(s).unwrap();
        e.remember(&shell("make"), GrantScope::Project).unwrap();
        assert!(dir.path().join(".harness/rules.toml").exists());
        assert_eq!(kind(&e.evaluate(&shell("make"))), "allow");
    }

    #[test]
    fn read_only_calls_need_nothing_remembered() {
        let e = engine(Profile::Auto);
        assert!(
            e.remember(&read("/proj/a"), GrantScope::Session)
                .unwrap()
                .contains("already allowed")
        );
    }

    #[test]
    fn invalid_default_deny_paths_are_reported() {
        let mut s = settings(Profile::Auto);
        s.default_deny_paths = vec!["a[".into()];
        let err = PolicyEngine::new(s).unwrap_err();
        assert!(
            err.to_string().contains("policy.default_deny_paths"),
            "{err}"
        );
    }
}
