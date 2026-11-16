//! The `harness` binary's library half.
//!
//! In later phases this crate wires the adapters (providers, stores, tools,
//! sandbox) into the runtime and hosts the CLI commands. In Phase 0 it only
//! provides [`telemetry`]: file logging filtered by `HARNESS_LOG`.
//!
//! # Example
//! ```
//! use harness_cli::telemetry::{filter_from, DEFAULT_FILTER};
//! assert_eq!(filter_from(None).unwrap().to_string(), DEFAULT_FILTER);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod telemetry;
