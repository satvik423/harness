//! Terminal UI for the harness (Phase 4, Tech spec 3.16), built purely on the
//! session event stream (guardrail 15).
//!
//! - [`App`] holds all state and transitions, free of I/O; events go in through
//!   [`App::apply`], keys through [`App::key`]
//! - [`view`] draws the screen (header, transcript with tool cards and diffs, approval
//!   panel, input box, status bar, overlays)
//! - [`Backend`] is what the TUI needs from harness; the `cli` crate implements it
//! - [`run`] runs the terminal event loop
//! - helpers are [`markdown`] (pulldown-cmark + syntect), [`diff`] (similar),
//!   [`ledger`] (tokens and cost), [`slash`], [`paste`] (dropped files), [`notify`]
//!
//! # Example
//! ```
//! use harness_tui::slash::{parse, Slash};
//! assert_eq!(parse("/undo"), Some(Ok(Slash::Undo)));
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod app;
pub mod backend;
pub mod diff;
pub mod ledger;
pub mod markdown;
pub mod notify;
pub mod paste;
mod run;
pub mod slash;
pub mod view;

pub use app::App;
pub use backend::{Attached, Backend, ChangedFile, SessionView};
pub use run::{TuiOptions, run};
