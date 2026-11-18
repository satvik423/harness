//! Test support for the harness.
//!
//! - [`FakeProvider`]: a scripted [`harness_ports::Provider`] that records every request
//! - [`TempProject`]: a throwaway project directory
//! - [`FixedClock`]: a controllable [`harness_ports::Clock`]
//! - [`RecordingSink`]: an [`harness_ports::EventSink`] that keeps every event
//! - [`MemoryStore`]: in-memory event log, session store, snapshot store and blob store
//! - [`AskForChanges`]: a minimal [`harness_ports::Policy`] (reads allowed, the rest asks)
//! - [`invariants`]: transcript invariant checks (append-only I1, tool pairing I2, no prefill)
//! - [`arch`]: layering checks that keep the architecture honest (P0-T9)
//!
//! This crate is a dev-dependency only; production crates must never depend
//! on it (enforced by [`arch::harness_rules`]).
//!
//! # Example
//! ```
//! use harness_testkit::{FakeProvider, TempProject};
//! let project = TempProject::new().unwrap().with_file("README.md", "# demo").unwrap();
//! let provider = FakeProvider::new().with_text_turn("Done.");
//! assert!(project.path().join("README.md").exists());
//! assert_eq!(provider.remaining_turns(), 1);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod arch;
mod clock;
mod fake_provider;
pub mod invariants;
mod policy;
mod project;
mod sink;
mod store;

pub use clock::FixedClock;
pub use fake_provider::FakeProvider;
pub use policy::AskForChanges;
pub use project::{TempProject, TempProjectError};
pub use sink::RecordingSink;
pub use store::MemoryStore;
