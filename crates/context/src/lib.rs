//! The context engine (Phase 1 subset): builds every model request.
//!
//! - [`SegmentContext`]: frozen system prompt and tools, plus an append-only
//!   transcript that refuses appends breaking invariants I1 and I2
//! - [`ActivePersona`], [`builtin_default`]: the persona settings used by a task
//! - [`load_project_instructions`]: the `AGENTS.md` chain (system slot 5)
//! - [`Environment`]: the environment block in each segment opener
//! - [`cap_output`]: the capped form of a tool result, decided once when recorded
//!
//! Request layout (Tech spec 2.6): system = base prompt, soul, instructions
//! (slots 1, 2, 5; skills and memory arrive in later phases); tools sorted by
//! name; first user message = opener + user input; then the conversation.
//!
//! # Example
//! ```
//! use harness_context::{SegmentContext, SegmentSpec, builtin_default};
//! use harness_types::ContentBlock;
//!
//! let mut segment = SegmentContext::open(SegmentSpec {
//!     persona: builtin_default(),
//!     tools: vec![],
//!     instructions: None,
//!     environment: "<environment>…</environment>".into(),
//!     segment: 0,
//! });
//! segment.push_user(vec![ContentBlock::text("hello")]).unwrap();
//! assert_eq!(segment.request().messages.len(), 1);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod capper;
mod date;
mod environment;
mod error;
mod instructions;
mod persona;
mod prompt;
mod segment;

pub use capper::{CHARS_PER_TOKEN, cap_output, estimate_tokens};
pub use date::civil_date;
pub use environment::{Environment, GitInfo};
pub use error::ContextError;
pub use instructions::{
    InstructionFile, MAX_INSTRUCTION_BYTES, ProjectInstructions, find_git_root,
    load_project_instructions,
};
pub use persona::{ActivePersona, builtin_default};
pub use prompt::{BASE_PROMPT, BASE_PROMPT_VERSION, DEFAULT_SOUL};
pub use segment::{SegmentContext, SegmentSpec};
