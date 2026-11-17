//! Anthropic Messages API provider (raw HTTP + SSE, ADR-012).
//!
//! [`AnthropicProvider`] implements [`harness_ports::Provider`]:
//!
//! - streaming `POST /v1/messages` with `anthropic-version: 2026-10-21`
//! - adaptive thinking, effort, cache breakpoints (system, opener, tail),
//!   server-side refusal fallbacks and preserved-thinking binding controls
//! - signed thinking blocks replayed verbatim as opaque content
//! - retries with exponential backoff and `retry-after` before streaming starts
//! - capabilities from `GET /v1/models/{id}`, token counts from
//!   `POST /v1/messages/count_tokens`
//!
//! # Example
//! ```no_run
//! use harness_provider_anthropic::{AnthropicProvider, AnthropicSettings, ApiKey};
//! let provider = AnthropicProvider::new(AnthropicSettings::default(), ApiKey::new("sk-ant-…")).unwrap();
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod decode;
mod http;
mod models;
mod provider;
mod request;
mod settings;

pub use decode::StreamDecoder;
pub use harness_ports::ApiKey;
pub use models::parse_capabilities;
pub use provider::AnthropicProvider;
pub use request::{PROVIDER_ID, build_count_body, build_messages_body};
pub use settings::{
    AnthropicSettings, SERVER_FALLBACK_BETA, THINKING_BINDING_BETA, ThinkingMismatch, beta_header,
};
