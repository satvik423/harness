//! Adapter settings.

use std::time::Duration;

/// Settings for [`crate::OpenAiProvider`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenAiSettings {
    /// API base URL (tests point this at a mock server).
    pub base_url: String,
    /// Ask OpenAI to keep responses server-side. The harness owns the history,
    /// so this stays `false` unless the user opts in.
    pub store: bool,
    /// Total attempts for a request that fails before streaming starts (FR-PRV-04).
    pub max_attempts: u32,
    /// Fail if no bytes arrive for this long. Reasoning models can think for a
    /// while before the first event, so this is generous.
    pub read_timeout: Duration,
    /// TCP and TLS connect timeout.
    pub connect_timeout: Duration,
}

impl Default for OpenAiSettings {
    fn default() -> Self {
        Self {
            base_url: "https://api.openai.com".into(),
            store: false,
            max_attempts: 4,
            read_timeout: Duration::from_secs(600),
            connect_timeout: Duration::from_secs(15),
        }
    }
}
