//! The `harness` binary's library half.
//!
//! Wires the adapters (provider, tools, in-memory stores) into the runtime and
//! hosts the CLI commands:
//!
//! - [`run`]: `harness run PROMPT` (headless, approvals on the terminal)
//! - [`render`]: human and JSON output, exit codes
//! - [`secrets`]: API keys from the keyring or environment
//! - [`wiring`]: the provider for the chosen model
//! - [`telemetry`]: file logging filtered by `HARNESS_LOG`
//!
//! # Example
//! ```
//! use harness_cli::telemetry::{filter_from, DEFAULT_FILTER};
//! assert_eq!(filter_from(None).unwrap().to_string(), DEFAULT_FILTER);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod render;
pub mod run;
pub mod secrets;
pub mod telemetry;
pub mod wiring;
