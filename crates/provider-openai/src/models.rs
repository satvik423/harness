//! Capabilities for OpenAI models.
//!
//! The Models API only confirms that a model exists (`id`, `owned_by`); it does
//! not report limits, so these are conservative defaults.

use harness_ports::{CacheStyle, Capabilities};

/// Capabilities used for every OpenAI model.
#[must_use]
pub fn default_capabilities() -> Capabilities {
    Capabilities {
        context_window: 200_000,
        max_output: 64_000,
        vision: true,
        pdf: true,
        parallel_tools: true,
        // Notes are sent as `developer` messages, which work anywhere in the input.
        mid_conversation_system: true,
        preserved_reasoning: true,
        cache: CacheStyle::AutomaticPrefix,
        structured_output: true,
        native_compaction: false,
        native_context_editing: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn caching_is_automatic_and_reasoning_is_preserved() {
        let caps = default_capabilities();
        assert_eq!(caps.cache, CacheStyle::AutomaticPrefix);
        assert!(caps.preserved_reasoning);
    }
}
