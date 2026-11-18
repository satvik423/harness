//! The `harness` binary's library half.
//!
//! Wires the adapters (provider, tools, policy, SQLite store) into the runtime
//! and hosts the CLI commands:
//!
//! - [`run`]: driving a session from the terminal (`run`, `session resume|undo`,
//!   `task resume|cancel`): approvals, questions, Ctrl-C, exit codes
//! - [`sessions`]: tables, event lines, exports and id prefixes for `session` and `task`
//! - [`prompt`]: parsing approval and question answers
//! - [`render`]: human and JSON output, exit codes
//! - [`secrets`]: API keys from the keyring or environment
//! - [`wiring`]: the provider for the chosen model and the policy engine
//! - [`telemetry`]: file logging filtered by `HARNESS_LOG`
//! - [`tui`]: the terminal UI's backend over the runtime and store; [`factory`]:
//!   building session dependencies; [`debug`]: request bodies and the append-only diff
//!
//! # Example
//! ```
//! use harness_cli::telemetry::{filter_from, DEFAULT_FILTER};
//! assert_eq!(filter_from(None).unwrap().to_string(), DEFAULT_FILTER);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod debug;
pub mod factory;
pub mod prompt;
pub mod render;
pub mod run;
pub mod secrets;
pub mod sessions;
pub mod telemetry;
pub mod tui;
pub mod wiring;
