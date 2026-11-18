//! The harness protocol.
//!
//! Front-ends talk to the core only through this crate:
//!
//! - [`Op`]: requests going **into** the core (submit input, pause, approve, …)
//! - [`Event`]: everything coming **out** of it, wrapped in an [`Envelope`]
//!   with a per-session sequence number
//!
//! Libraries never print; they emit events (SDD principle P4). The same events
//! are stored in the event log, so a session can be replayed exactly.
//!
//! # Example
//! ```
//! use harness_protocol::{Event, Op};
//! use harness_types::TaskId;
//!
//! let op = Op::Pause { task: TaskId::new() };
//! let json = serde_json::to_string(&op).unwrap();
//! assert!(json.contains("\"op\":\"pause\""));
//!
//! let delta = Event::TextDelta { task: TaskId::new(), text: "Hi".into() };
//! assert!(!delta.is_persisted());
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod compaction;
mod event;
mod op;

pub use compaction::{Anchor, CompactionRecord, KeyDecision, PinKind, PinnedFact};
pub use event::{BlobOrText, Envelope, Event, HookOutcome, TaskState};
pub use op::{Decision, InputItem, Op, RuleScope};
