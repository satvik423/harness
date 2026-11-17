//! OpenAI Responses API provider (raw HTTP + SSE).
//!
//! [`OpenAiProvider`] implements [`harness_ports::Provider`]:
//!
//! - streaming `POST /v1/responses` with `store: false` (the harness owns the
//!   history) and `include: ["reasoning.encrypted_content"]`
//! - reasoning items replayed verbatim as opaque content
//! - function tools (strict when the schema allows), reasoning effort and summaries
//! - retries with exponential backoff and `retry-after` before streaming starts
//!
//! # Example
//! ```no_run
//! use harness_provider_openai::{ApiKey, OpenAiProvider, OpenAiSettings};
//! let provider = OpenAiProvider::new(OpenAiSettings::default(), ApiKey::new("sk-…")).unwrap();
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod decode;
mod http;
mod models;
mod provider;
mod request;
mod settings;

pub use decode::ResponsesDecoder;
pub use harness_ports::ApiKey;
pub use models::default_capabilities;
pub use provider::OpenAiProvider;
pub use request::{PROVIDER_ID, build_responses_body};
pub use settings::OpenAiSettings;
