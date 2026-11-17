//! The harness runtime: the session actor and the agent loop (SDD 2.5, C1).
//!
//! [`spawn_session`] starts one actor per session. Front-ends send
//! [`harness_protocol::Op`]s through the returned [`SessionHandle`] and read
//! [`harness_protocol::Envelope`]s from the event receiver. Each submitted input
//! becomes a task that runs the loop: build the request (via
//! [`harness_context::SegmentContext`]), stream the reply, run tool calls
//! (asking for approval when a call is not read-only), append every result in
//! one message, and repeat until the model stops calling tools.
//!
//! The runtime depends only on ports, never on concrete adapters (ADR-001, ADR-016).
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod approval;
mod session;
mod sink;
mod turn;

pub use approval::ApprovalMode;
pub use session::{RuntimeDeps, RuntimeError, SessionConfig, SessionHandle, spawn_session};
pub use sink::BroadcastSink;
