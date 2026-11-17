//! [`ActivePersona`]: the persona settings the context engine needs, and the
//! built-in `default` persona (until persona files arrive in Phase 6).

use harness_ports::Effort;
use harness_types::ModelRef;

use crate::prompt::DEFAULT_SOUL;

/// The resolved persona for a task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivePersona {
    /// Persona name, for example `default`.
    pub name: String,
    /// The persona's "soul": its part of the system prompt (slot 2).
    pub soul: String,
    /// Model to use.
    pub model: ModelRef,
    /// Reasoning effort; `None` uses the provider default.
    pub effort: Option<Effort>,
    /// Maximum output tokens per model call.
    pub max_output: u64,
    /// Stream readable reasoning summaries.
    pub show_thinking: bool,
}

/// The built-in `default` persona: Claude Opus 5.5 at high effort.
///
/// # Examples
/// ```
/// let p = harness_context::builtin_default();
/// assert_eq!(p.name, "default");
/// assert_eq!(p.model.to_string(), "anthropic/claude-opus-5-5");
/// ```
#[must_use]
pub fn builtin_default() -> ActivePersona {
    ActivePersona {
        name: "default".into(),
        soul: DEFAULT_SOUL.into(),
        model: "anthropic/claude-opus-5-5"
            .parse()
            .expect("the built-in model reference is valid"),
        // Opus 5.5 defaults to medium; agentic work benefits from high.
        effort: Some(Effort::High),
        max_output: 64_000,
        show_thinking: false,
    }
}
