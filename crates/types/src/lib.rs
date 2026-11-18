//! Shared domain types for the harness.
//!
//! Every other crate depends on this one, and this crate depends on no other
//! harness crate. It holds only data types and small, pure helpers:
//!
//! - identifiers: [`SessionId`], [`TaskId`], [`ApprovalId`], [`QuestionId`], [`Seq`], [`BlobRef`]
//! - model naming: [`ModelRef`], [`Effort`]
//! - conversation content: [`Role`], [`ContentBlock`], [`Message`]
//! - accounting: [`Usage`], [`StopReason`]
//! - safety: [`Risk`], [`Mode`]
//!
//! # Example
//! ```
//! use harness_types::{ContentBlock, Message, Role};
//! let reply = Message::new(Role::Assistant, vec![ContentBlock::text("Done.")]);
//! assert_eq!(reply.text(), "Done.");
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod effort;
mod ids;
mod message;
mod mode;
mod model_ref;
mod risk;
mod usage;

pub use effort::Effort;
pub use ids::{
    ApprovalId, BlobRef, ParseBlobRefError, ParseIdError, QuestionId, Seq, SessionId, TaskId,
};
pub use message::{ContentBlock, Message, Role};
pub use mode::{Mode, ParseModeError};
pub use model_ref::{ModelRef, ParseModelRefError};
pub use risk::Risk;
pub use usage::{StopReason, Usage};
