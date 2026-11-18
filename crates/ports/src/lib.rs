//! Port traits of the harness (SDD 2.3, layer L4).
//!
//! The runtime and context engine depend **only** on this crate (plus
//! `harness-types` and `harness-protocol`). Concrete adapters (providers,
//! stores, tools, sandboxes) implement these traits in their own crates and are
//! wired together in the `cli` binary (ADR-001).
//!
//! | Port | Purpose | Implemented in |
//! |---|---|---|
//! | [`Provider`] | one streamed model call | `provider-*` (Phase 1, 12) |
//! | [`Tool`] | something the model can call | `tools`, `mcp` (Phase 1+) |
//! | [`EventLog`] | append-only source of truth | `store-sqlite` (Phase 3) |
//! | [`BlobStore`] | content-addressed bytes | `store-sqlite` (Phase 3) |
//! | [`SessionStore`] | sessions, tasks, task controls | `store-sqlite` (Phase 3) |
//! | [`SnapshotStore`], [`FileSnapshots`] | file snapshots for undo | `store-sqlite`, `runtime` (Phase 3) |
//! | [`Sandbox`] | wraps process execution | `sandbox` (Phase 13) |
//! | [`ReadLedger`] | read-before-edit guard | `tools` (Phase 2) |
//! | [`EventSink`], [`Clock`] | live events, injectable time | `runtime`, `testkit` |
//! | [`UserChannel`] | a tool asks the user a question | `runtime` (Phase 2) |
//! | [`Policy`] | allow, ask or deny each tool call | `policy` (Phase 2) |
//!
//! Ports for hooks and memory are added in the phases that need them (7 and 14).
//!
//! # Example
//! ```
//! use harness_ports::{Clock, SystemClock};
//! fn stamp(clock: &dyn Clock) -> i64 { clock.now_ms() }
//! assert!(stamp(&SystemClock) > 0);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod credentials;
mod ledger;
mod policy;
mod provider;
mod registry;
mod retry;
mod runtime;
mod sandbox;
mod session_store;
mod snapshot;
mod store;
mod tool;
mod untrusted;
mod user;

pub use credentials::ApiKey;
pub use ledger::{FileStamp, LedgerCheck, ReadLedger};
pub use policy::{GrantScope, Policy, PolicyCall, PolicyError, Verdict};
pub use provider::{
    CachePlan, CacheStyle, Capabilities, Effort, ModelRequest, Provider, ProviderError,
    ProviderEvent, ProviderStream, SystemPart, SystemSlot,
};
pub use registry::{DuplicateTool, ToolRegistry};
pub use retry::{MAX_BACKOFF, MAX_RETRY_AFTER, backoff_delay};
pub use runtime::{Clock, EventSink, SystemClock};
pub use sandbox::{CommandSpec, Sandbox, SandboxError, SandboxMode};
pub use session_store::{SessionRecord, SessionStatus, SessionStore, TaskControl, TaskRecord};
pub use snapshot::{FileSnapshots, NoSnapshots, Snapshot, SnapshotStore, Undone};
pub use store::{BlobMeta, BlobStore, EventLog, LoggedEvent, SearchHit, StoreError};
pub use tool::{
    InvalidToolName, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
};
pub use untrusted::wrap_untrusted;
pub use user::{AskError, NoUser, UserChannel, UserQuestion};

#[cfg(test)]
mod object_safety {
    //! Every port must stay usable as a trait object (`dyn Port`), because the
    //! runtime holds adapters behind `Arc<dyn …>` (ADR-001).
    use super::*;

    fn assert_dyn_compatible<T: ?Sized>() {}

    #[test]
    fn every_port_is_dyn_compatible() {
        assert_dyn_compatible::<dyn Provider>();
        assert_dyn_compatible::<dyn Tool>();
        assert_dyn_compatible::<dyn EventLog>();
        assert_dyn_compatible::<dyn BlobStore>();
        assert_dyn_compatible::<dyn Sandbox>();
        assert_dyn_compatible::<dyn ReadLedger>();
        assert_dyn_compatible::<dyn EventSink>();
        assert_dyn_compatible::<dyn Clock>();
        assert_dyn_compatible::<dyn UserChannel>();
        assert_dyn_compatible::<dyn Policy>();
        assert_dyn_compatible::<dyn SessionStore>();
        assert_dyn_compatible::<dyn SnapshotStore>();
        assert_dyn_compatible::<dyn FileSnapshots>();
    }
}
