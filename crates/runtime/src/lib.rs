//! The harness runtime: the session actor and the agent loop (SDD 2.5, C1).
//!
//! [`spawn_session`] starts a new session and [`resume_session`] reopens a
//! stored one (rebuilding it from the event log and recovering a task that was
//! running when the harness stopped). Each runs one actor. Front-ends send
//! [`harness_protocol::Op`]s through the returned [`SessionHandle`] and read
//! [`harness_protocol::Envelope`]s from the event receiver. Each submitted input
//! becomes a task that runs the loop: build the request (via
//! [`harness_context::SegmentContext`]), stream the reply, run tool calls
//! (asking for approval when a call is not read-only), append every result in
//! one message, and repeat until the model stops calling tools.
//!
//! Every persisted event is committed to the event log before it is broadcast
//! and before anything that depends on it happens (NFR-03). Tasks can be
//! paused at safe points and resumed, even after a restart.
//!
//! The runtime depends only on ports, never on concrete adapters (ADR-001, ADR-016).
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod approval;
mod compact;
mod history;
mod journal;
mod memory;
mod mode;
mod pricing;
mod replay;
mod session;
mod snapshots;
mod turn;
mod user;

pub use approval::ApprovalMode;
pub use history::recall;
pub use memory::ContextSettings;
pub use pricing::ModelPrice;
pub use replay::{OUTCOME_UNKNOWN, RecordedRequest, request_history};
pub use session::{
    Forked, ModelSource, Resumed, RuntimeDeps, RuntimeError, SessionConfig, SessionHandle,
    fork_session, resume_session, spawn_session,
};
