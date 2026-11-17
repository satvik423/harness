//! The harness policy engine (P2-T5, Tech spec 3.6): decides whether each tool
//! call may run, must be approved, or is refused.
//!
//! - [`PolicyEngine`]: implements [`harness_ports::Policy`] from built-in deny
//!   paths, the user's and the (trusted) project's `rules.toml`, session
//!   approvals and the active [`Profile`]
//! - [`analyze`]: splits shell commands with tree-sitter-bash so each simple
//!   command is judged on its own, and flags constructs that must be shown to
//!   the user
//! - rules files: see [`RulesFile`] for the format
//!
//! The engine is a convenience layer that saves questions; the sandbox
//! (Phase 13) is the wall (guardrail 11).
//!
//! # Example
//! ```
//! use harness_policy::{PolicyEngine, PolicySettings, Profile};
//! use harness_ports::{Policy, PolicyCall, Verdict};
//! use harness_types::Risk;
//!
//! let engine = PolicyEngine::new(PolicySettings {
//!     profile: Profile::Auto,
//!     approval: Profile::Auto.default_approval(),
//!     sandboxed: false,
//!     default_deny_paths: vec!["**/.env*".into()],
//!     home: None,
//!     user_rules: None,
//!     project_rules: None,
//! })?;
//! let call = PolicyCall {
//!     tool: "read_file".into(),
//!     input: serde_json::json!({"path": ".env"}),
//!     risk: Risk::ReadOnly { paths: vec!["/proj/.env".into()] },
//!     cwd: "/proj".into(),
//!     writable_roots: vec!["/proj".into()],
//! };
//! assert!(matches!(engine.evaluate(&call), Verdict::Deny { .. }));
//! # Ok::<(), harness_policy::RuleError>(())
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod engine;
mod rules;
mod shell;

pub use engine::{ApprovalPolicy, PolicyEngine, PolicySettings, Profile};
pub use rules::{Decision, Origin, RawRule, RuleError, RulesFile};
pub use shell::{Analysis, Part, analyze};
