//! Capabilities from `GET /v1/models/{id}`.

use harness_ports::{CacheStyle, Capabilities};
use serde_json::Value;

/// Builds [`Capabilities`] from a Models API object, with conservative defaults
/// for fields the response does not carry.
#[must_use]
pub fn parse_capabilities(model: &Value) -> Capabilities {
    let id = model["id"].as_str().unwrap_or_default();
    // Families that accept `role: "system"` messages mid-conversation.
    let mid_conversation_system = [
        "claude-opus-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-fable",
        "claude-mythos",
        "claude-opus-4-8",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix));
    Capabilities {
        context_window: model["max_input_tokens"].as_u64().unwrap_or(200_000),
        max_output: model["max_tokens"].as_u64().unwrap_or(32_000),
        vision: true,
        pdf: true,
        parallel_tools: true,
        mid_conversation_system,
        preserved_reasoning: true,
        cache: CacheStyle::Explicit { max_breakpoints: 4 },
        structured_output: true,
        native_compaction: true,
        native_context_editing: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn limits_come_from_the_response() {
        let caps = parse_capabilities(
            &json!({"id": "claude-opus-5-5", "max_input_tokens": 1_000_000, "max_tokens": 128_000}),
        );
        assert_eq!((caps.context_window, caps.max_output), (1_000_000, 128_000));
        assert!(caps.preserved_reasoning);
        assert_eq!(caps.cache, CacheStyle::Explicit { max_breakpoints: 4 });
    }

    #[test]
    fn missing_fields_fall_back_to_safe_defaults() {
        let caps = parse_capabilities(&json!({"id": "claude-x"}));
        assert_eq!((caps.context_window, caps.max_output), (200_000, 32_000));
    }

    #[test]
    fn mid_conversation_system_messages_follow_the_model_family() {
        for (id, expected) in [
            ("claude-opus-5-5", true),
            ("claude-sonnet-5-5", true),
            ("claude-haiku-5-5", true),
            ("claude-sonnet-5", false),
            ("claude-haiku-4-5", false),
        ] {
            assert_eq!(
                parse_capabilities(&json!({"id": id})).mid_conversation_system,
                expected,
                "{id}"
            );
        }
    }
}
