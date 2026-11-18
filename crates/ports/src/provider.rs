//! The [`Provider`] port: one model call, streamed, in a provider-neutral form.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use harness_types::{ContentBlock, Message, StopReason, Usage};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use crate::tool::ToolSpec;

/// A stream of provider events for one model call.
pub type ProviderStream = BoxStream<'static, Result<ProviderEvent, ProviderError>>;

/// A model backend (Anthropic Messages API, OpenAI Responses API, a test fake, …).
///
/// Adapters translate [`ModelRequest`] to their wire format and map the streamed
/// reply back into [`ProviderEvent`]s. Provider-specific blocks that must be
/// replayed verbatim (signed thinking, encrypted reasoning) are emitted as
/// opaque blocks inside [`ProviderEvent::BlockComplete`].
#[async_trait]
pub trait Provider: Send + Sync {
    /// Stable provider id, for example `anthropic` or `openai`.
    fn id(&self) -> &str;

    /// What `model` supports. Adapters should read this from the provider's
    /// models endpoint where one exists (FR-PRV-03) and cache it.
    ///
    /// # Errors
    /// Returns a [`ProviderError`] if the capabilities cannot be fetched.
    async fn capabilities(&self, model: &str) -> Result<Capabilities, ProviderError>;

    /// Starts one streamed model call. Cancelling `cancel` must end the stream promptly.
    ///
    /// # Errors
    /// Returns a [`ProviderError`] if the call cannot be started; errors during
    /// streaming arrive as `Err` items in the stream.
    async fn stream(
        &self,
        request: ModelRequest,
        cancel: CancellationToken,
    ) -> Result<ProviderStream, ProviderError>;

    /// Counts the input tokens `request` would use.
    ///
    /// # Errors
    /// Returns a [`ProviderError`] if counting fails.
    async fn count_tokens(&self, request: &ModelRequest) -> Result<u64, ProviderError>;
}

/// How a provider caches prompt prefixes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "style", rename_all = "snake_case")]
pub enum CacheStyle {
    /// Caller places explicit breakpoints (Anthropic `cache_control`), up to a maximum.
    Explicit {
        /// Maximum breakpoints per request.
        max_breakpoints: u8,
    },
    /// The provider caches stable prefixes automatically.
    AutomaticPrefix,
    /// No prompt caching.
    None,
}

/// What a model supports. Code asks capabilities, never provider names (SDD P5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    /// Maximum input tokens.
    pub context_window: u64,
    /// Maximum output tokens per call.
    pub max_output: u64,
    /// Accepts image input.
    pub vision: bool,
    /// Accepts PDF input natively.
    pub pdf: bool,
    /// Can request several tool calls in one reply.
    pub parallel_tools: bool,
    /// Accepts `role: "system"` messages inside the conversation.
    pub mid_conversation_system: bool,
    /// Opaque reasoning blocks must round-trip and history must stay append-only.
    pub preserved_reasoning: bool,
    /// Prompt caching style.
    pub cache: CacheStyle,
    /// Supports JSON-schema constrained final output.
    pub structured_output: bool,
    /// Offers server-side compaction.
    pub native_compaction: bool,
    /// Offers server-side context editing (tool-result clearing).
    pub native_context_editing: bool,
}

pub use harness_types::Effort;

/// Which part of the frozen system prompt a [`SystemPart`] is (Tech spec 2.6, slots 1–6).
///
/// The derived ordering is the order the parts are sent in. Tool definitions
/// (slot 3) travel separately in [`ModelRequest::tools`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemSlot {
    /// Slot 1: the harness base prompt.
    Base,
    /// Slot 2: the persona soul.
    Soul,
    /// Slot 4: the skills index.
    Skills,
    /// Slot 5: project instructions (AGENTS.md chain).
    Instructions,
    /// Slot 6: the memory index.
    Memory,
}

/// One part of the system prompt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemPart {
    /// Which slot this is.
    pub slot: SystemSlot,
    /// The text.
    pub text: String,
}

/// Where cache breakpoints go (used by providers with [`CacheStyle::Explicit`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CachePlan {
    /// Breakpoint at the end of the system prompt.
    pub system_breakpoint: bool,
    /// Breakpoint after the segment opener message.
    pub opener_breakpoint: bool,
    /// Automatic caching of the growing tail.
    pub auto_tail: bool,
}

impl Default for CachePlan {
    fn default() -> Self {
        Self {
            system_breakpoint: true,
            opener_breakpoint: true,
            auto_tail: true,
        }
    }
}

/// A provider-neutral model call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelRequest {
    /// The provider's model id (already routed; no `provider/` prefix).
    pub model: String,
    /// Frozen system prompt parts, in slot order.
    pub system: Vec<SystemPart>,
    /// Tool definitions, sorted by name, frozen per segment.
    pub tools: Vec<ToolSpec>,
    /// The append-only transcript of the current segment.
    pub messages: Vec<Message>,
    /// Maximum output tokens.
    pub max_output: u64,
    /// Reasoning effort, if the model supports it.
    pub effort: Option<Effort>,
    /// Cache breakpoint placement.
    pub cache_plan: CachePlan,
    /// JSON schema the final answer must match, if any.
    pub output_schema: Option<Value>,
    /// Ask for a readable reasoning summary to be streamed.
    pub show_thinking: bool,
}

/// One item of a streamed model reply.
///
/// The runtime builds the assistant message **only** from
/// [`ProviderEvent::BlockComplete`] events, in arrival order, so the transcript
/// matches what the model produced exactly (required for preserved reasoning,
/// ADR-003). The delta and start events exist for live display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderEvent {
    /// Visible text fragment.
    TextDelta {
        /// The fragment.
        text: String,
    },
    /// Reasoning summary fragment.
    ThinkingDelta {
        /// The fragment.
        text: String,
    },
    /// A tool call began (display only; the call itself arrives in [`ProviderEvent::BlockComplete`]).
    ToolCallStart {
        /// Provider call id.
        id: String,
        /// Tool name.
        name: String,
    },
    /// A fragment of a tool call's JSON input.
    ToolCallInputDelta {
        /// Provider call id.
        id: String,
        /// Raw JSON fragment.
        json: String,
    },
    /// A content block is complete: text, a tool call with parsed input, or an
    /// opaque provider block (signed thinking, encrypted reasoning) to replay verbatim.
    BlockComplete {
        /// The finished block, exactly as it must appear in the transcript.
        block: ContentBlock,
    },
    /// Token usage for the call (may arrive more than once; the last one wins).
    Usage {
        /// The counts.
        usage: Usage,
    },
    /// The model stopped.
    Stop {
        /// Why.
        reason: StopReason,
    },
    /// Something the user should know, for example "thinking blocks dropped".
    Notice {
        /// The message.
        message: String,
    },
}

/// Errors from a provider adapter.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ProviderError {
    /// HTTP 429.
    #[error("rate limited")]
    RateLimited {
        /// Server-suggested wait, from `retry-after`.
        retry_after_ms: Option<u64>,
    },
    /// The provider is overloaded (for example HTTP 529).
    #[error("provider overloaded")]
    Overloaded {
        /// Server-suggested wait, from `retry-after`.
        retry_after_ms: Option<u64>,
    },
    /// HTTP 5xx other than overload.
    #[error("server error {status}: {message}")]
    Server {
        /// HTTP status.
        status: u16,
        /// Error body or summary.
        message: String,
    },
    /// Connection failed or dropped.
    #[error("network error: {0}")]
    Network(String),
    /// The request timed out.
    #[error("request timed out")]
    Timeout,
    /// Missing or rejected credentials.
    #[error("authentication failed: {0}")]
    Auth(String),
    /// The provider rejected the request (HTTP 4xx other than 401, 403, 429).
    #[error("invalid request ({status}): {message}")]
    InvalidRequest {
        /// HTTP status.
        status: u16,
        /// Error body or summary.
        message: String,
    },
    /// The response could not be decoded.
    #[error("could not decode provider response: {0}")]
    Decode(String),
    /// The call was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// The model or provider does not support something the request needs.
    #[error("unsupported: {0}")]
    Unsupported(String),
}

impl ProviderError {
    /// `true` for transient failures worth retrying with backoff (FR-PRV-04):
    /// rate limits, overload, 5xx, network errors and timeouts.
    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self,
            Self::RateLimited { .. }
                | Self::Overloaded { .. }
                | Self::Server { .. }
                | Self::Network(_)
                | Self::Timeout
        )
    }

    /// The server-suggested wait before retrying, if it sent one.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::RateLimited { retry_after_ms } | Self::Overloaded { retry_after_ms } => {
                retry_after_ms.map(Duration::from_millis)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transient_errors_are_retryable() {
        for e in [
            ProviderError::RateLimited {
                retry_after_ms: None,
            },
            ProviderError::Overloaded {
                retry_after_ms: None,
            },
            ProviderError::Server {
                status: 502,
                message: String::new(),
            },
            ProviderError::Network("reset".into()),
            ProviderError::Timeout,
        ] {
            assert!(e.is_retryable(), "{e:?}");
        }
    }

    #[test]
    fn permanent_errors_are_not_retryable() {
        for e in [
            ProviderError::Auth("bad key".into()),
            ProviderError::InvalidRequest {
                status: 400,
                message: String::new(),
            },
            ProviderError::Decode("x".into()),
            ProviderError::Cancelled,
            ProviderError::Unsupported("pdf".into()),
        ] {
            assert!(!e.is_retryable(), "{e:?}");
        }
    }

    #[test]
    fn retry_after_comes_from_rate_limit_and_overload_only() {
        assert_eq!(
            ProviderError::RateLimited {
                retry_after_ms: Some(1500)
            }
            .retry_after(),
            Some(Duration::from_millis(1500))
        );
        assert_eq!(
            ProviderError::Overloaded {
                retry_after_ms: Some(20)
            }
            .retry_after(),
            Some(Duration::from_millis(20))
        );
        assert_eq!(ProviderError::Timeout.retry_after(), None);
    }

    #[test]
    fn effort_serialises_as_api_level_names() {
        let names: Vec<String> = [
            Effort::Low,
            Effort::Medium,
            Effort::High,
            Effort::Xhigh,
            Effort::Max,
        ]
        .iter()
        .map(|e| {
            serde_json::to_value(e)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
        assert_eq!(names, ["low", "medium", "high", "xhigh", "max"]);
    }

    #[test]
    fn system_slots_sort_in_send_order() {
        let mut slots = vec![
            SystemSlot::Memory,
            SystemSlot::Base,
            SystemSlot::Instructions,
            SystemSlot::Skills,
            SystemSlot::Soul,
        ];
        slots.sort();
        assert_eq!(
            slots,
            vec![
                SystemSlot::Base,
                SystemSlot::Soul,
                SystemSlot::Skills,
                SystemSlot::Instructions,
                SystemSlot::Memory
            ]
        );
    }

    #[test]
    fn block_complete_round_trips_with_its_block() {
        let e = ProviderEvent::BlockComplete {
            block: ContentBlock::text("hi"),
        };
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["type"], "block_complete");
        assert_eq!(serde_json::from_value::<ProviderEvent>(v).unwrap(), e);
    }

    #[test]
    fn cache_plan_defaults_to_all_breakpoints() {
        assert_eq!(
            CachePlan::default(),
            CachePlan {
                system_breakpoint: true,
                opener_breakpoint: true,
                auto_tail: true
            }
        );
    }
}
