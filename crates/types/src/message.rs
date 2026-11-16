//! Conversation content: [`Role`], [`ContentBlock`] and [`Message`].

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::BlobRef;

/// Who a [`Message`] is from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    /// The user, or tool results returned to the model.
    User,
    /// The model.
    Assistant,
    /// An operator note appended mid-conversation (never edited or removed later).
    System,
}

/// One piece of message content, in a provider-neutral form.
///
/// Provider adapters translate these to and from their wire formats.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentBlock {
    /// Plain text.
    Text {
        /// The text.
        text: String,
    },
    /// An image stored in the blob store.
    Image {
        /// Content address of the image bytes.
        blob: BlobRef,
        /// MIME type, for example `image/png`.
        media_type: String,
    },
    /// A document (for example a PDF) stored in the blob store.
    Document {
        /// Content address of the document bytes.
        blob: BlobRef,
        /// MIME type, for example `application/pdf`.
        media_type: String,
        /// Optional human-readable title.
        title: Option<String>,
    },
    /// A tool call requested by the model.
    ToolCall {
        /// Provider-assigned call id; the matching [`ContentBlock::ToolResult`] carries it.
        id: String,
        /// Tool name.
        name: String,
        /// Tool input, already parsed from JSON.
        input: Value,
    },
    /// The result of a tool call, sent back to the model.
    ToolResult {
        /// The id of the [`ContentBlock::ToolCall`] this answers.
        call_id: String,
        /// Result content (usually text, possibly images).
        content: Vec<ContentBlock>,
        /// `true` if the tool failed; the model still sees the content.
        is_error: bool,
    },
    /// Provider data that MUST round-trip byte-for-byte within its segment:
    /// Anthropic `thinking` / `redacted_thinking` blocks (with signature) and
    /// OpenAI reasoning items (with `encrypted_content`).
    Opaque {
        /// Provider id that produced the block, for example `anthropic`.
        provider: String,
        /// Model id that produced the block.
        model: String,
        /// The block exactly as received.
        raw: Value,
    },
}

impl ContentBlock {
    /// Convenience constructor for [`ContentBlock::Text`].
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }
}

/// One message in a transcript.
///
/// # Examples
/// ```
/// use harness_types::{Message, Role};
/// let m = Message::user_text("hello");
/// assert_eq!(m.role, Role::User);
/// assert_eq!(m.text(), "hello");
/// ```
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    /// Who the message is from.
    pub role: Role,
    /// The content blocks, in order.
    pub content: Vec<ContentBlock>,
}

impl Message {
    /// Creates a message from a role and content blocks.
    #[must_use]
    pub fn new(role: Role, content: Vec<ContentBlock>) -> Self {
        Self { role, content }
    }

    /// Creates a user message containing a single text block.
    #[must_use]
    pub fn user_text(text: impl Into<String>) -> Self {
        Self::new(Role::User, vec![ContentBlock::text(text)])
    }

    /// Concatenates all [`ContentBlock::Text`] blocks, separated by newlines.
    /// Other block kinds are ignored.
    #[must_use]
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Ids of every [`ContentBlock::ToolCall`] in this message, in order.
    #[must_use]
    pub fn tool_call_ids(&self) -> Vec<&str> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    }

    /// Ids answered by every [`ContentBlock::ToolResult`] in this message, in order.
    #[must_use]
    pub fn tool_result_ids(&self) -> Vec<&str> {
        self.content
            .iter()
            .filter_map(|b| match b {
                ContentBlock::ToolResult { call_id, .. } => Some(call_id.as_str()),
                _ => None,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn call(id: &str) -> ContentBlock {
        ContentBlock::ToolCall {
            id: id.into(),
            name: "shell".into(),
            input: json!({"command": "ls"}),
        }
    }

    fn result(id: &str) -> ContentBlock {
        ContentBlock::ToolResult {
            call_id: id.into(),
            content: vec![ContentBlock::text("ok")],
            is_error: false,
        }
    }

    #[test]
    fn user_text_builds_single_text_block() {
        let m = Message::user_text("hi");
        assert_eq!(m, Message::new(Role::User, vec![ContentBlock::text("hi")]));
    }

    #[test]
    fn text_joins_text_blocks_and_skips_others() {
        let m = Message::new(
            Role::Assistant,
            vec![ContentBlock::text("a"), call("c1"), ContentBlock::text("b")],
        );
        assert_eq!(m.text(), "a\nb");
    }

    #[test]
    fn tool_ids_are_collected_in_order() {
        let a = Message::new(
            Role::Assistant,
            vec![call("c1"), ContentBlock::text("x"), call("c2")],
        );
        let u = Message::new(Role::User, vec![result("c2"), result("c1")]);
        assert_eq!(a.tool_call_ids(), vec!["c1", "c2"]);
        assert_eq!(u.tool_result_ids(), vec!["c2", "c1"]);
        assert!(a.tool_result_ids().is_empty());
    }

    #[test]
    fn content_blocks_use_snake_case_type_tags() {
        let v = serde_json::to_value(call("c1")).unwrap();
        assert_eq!(v["type"], "tool_call");
        let v = serde_json::to_value(result("c1")).unwrap();
        assert_eq!(v["type"], "tool_result");
    }

    #[test]
    fn opaque_block_round_trips_raw_value_unchanged() {
        let raw = json!({"type": "thinking", "thinking": "", "signature": "EqQBCkgIARABGAIi..."});
        let block = ContentBlock::Opaque {
            provider: "anthropic".into(),
            model: "claude-opus-5-5".into(),
            raw: raw.clone(),
        };
        let json = serde_json::to_string(&block).unwrap();
        let back: ContentBlock = serde_json::from_str(&json).unwrap();
        assert_eq!(back, block);
        if let ContentBlock::Opaque { raw: r, .. } = back {
            assert_eq!(r, raw);
        }
    }

    #[test]
    fn message_round_trips_through_json() {
        let m = Message::new(
            Role::System,
            vec![ContentBlock::Image {
                blob: BlobRef::from_bytes([7; 32]),
                media_type: "image/png".into(),
            }],
        );
        let back: Message = serde_json::from_str(&serde_json::to_string(&m).unwrap()).unwrap();
        assert_eq!(back, m);
    }
}
