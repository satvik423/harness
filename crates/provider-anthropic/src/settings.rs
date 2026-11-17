//! Adapter settings and the API key wrapper.

use std::time::Duration;

/// Beta enabling the preserved-thinking mismatch controls (`thinking.block_binding`).
pub const THINKING_BINDING_BETA: &str = "thinking-binding-controls-2026-08-01";
/// Beta enabling server-side refusal fallbacks (`fallbacks: "default"`).
pub const SERVER_FALLBACK_BETA: &str = "server-side-fallback-2026-07-01";

/// What the API should do when a replayed thinking block no longer matches its conversation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThinkingMismatch {
    /// Reject the request (use in tests and CI so history edits are caught).
    Error,
    /// Drop the affected thinking blocks and continue (production).
    DropBlock,
}

impl ThinkingMismatch {
    /// The wire value for `prefix_mismatch_behavior`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::DropBlock => "drop_block",
        }
    }
}

/// Settings for [`crate::AnthropicProvider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnthropicSettings {
    /// API base URL (tests point this at a mock server).
    pub base_url: String,
    /// `anthropic-version` header.
    pub api_version: String,
    /// Extra opt-in betas.
    pub betas: Vec<String>,
    /// Send `fallbacks: "default"` (server-side refusal fallback).
    pub server_fallbacks: bool,
    /// Preserved-thinking mismatch handling.
    pub thinking_mismatch: ThinkingMismatch,
    /// Total attempts for a request that fails before streaming starts (FR-PRV-04).
    pub max_attempts: u32,
    /// Fail if no bytes arrive for this long while streaming.
    pub read_timeout: Duration,
    /// TCP and TLS connect timeout.
    pub connect_timeout: Duration,
}

impl Default for AnthropicSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.anthropic.com".into(),
            api_version: "2026-10-21".into(),
            betas: Vec::new(),
            server_fallbacks: true,
            thinking_mismatch: ThinkingMismatch::DropBlock,
            max_attempts: 4,
            read_timeout: Duration::from_secs(120),
            connect_timeout: Duration::from_secs(15),
        }
    }
}

/// The comma-separated `anthropic-beta` header value for these settings.
#[must_use]
pub fn beta_header(settings: &AnthropicSettings) -> String {
    let mut betas: Vec<&str> = vec![THINKING_BINDING_BETA];
    if settings.server_fallbacks {
        betas.push(SERVER_FALLBACK_BETA);
    }
    for beta in &settings.betas {
        if !betas.contains(&beta.as_str()) {
            betas.push(beta);
        }
    }
    betas.join(",")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn beta_header_includes_binding_and_fallback_without_duplicates() {
        let mut s = AnthropicSettings {
            betas: vec![THINKING_BINDING_BETA.into(), "extra-2026-01-01".into()],
            ..AnthropicSettings::default()
        };
        assert_eq!(
            beta_header(&s),
            format!("{THINKING_BINDING_BETA},{SERVER_FALLBACK_BETA},extra-2026-01-01")
        );
        s.server_fallbacks = false;
        assert!(!beta_header(&s).contains(SERVER_FALLBACK_BETA));
    }

    #[test]
    fn defaults_match_the_spec() {
        let s = AnthropicSettings::default();
        assert_eq!(s.api_version, "2026-10-21");
        assert_eq!(s.max_attempts, 4);
        assert_eq!(s.thinking_mismatch.as_str(), "drop_block");
    }
}
