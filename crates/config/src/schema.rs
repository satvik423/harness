//! The typed configuration schema (Tech spec 3.17) and its defaults.

use std::collections::BTreeMap;

use harness_types::ModelRef;
use serde::{Deserialize, Serialize};

use crate::{ConfigError, SecretRef};

/// The complete, merged configuration.
///
/// Unknown keys are rejected so typos surface as errors instead of being
/// silently ignored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    /// Persona used when none is given.
    pub default_persona: String,
    /// Model for the built-in `default` persona (and `harness run` without `--model`).
    pub default_model: ModelRef,
    /// Model provider settings.
    pub providers: Providers,
    /// Context engine settings.
    pub context: ContextConfig,
    /// Permission settings.
    pub policy: PolicyConfig,
    /// Sandbox settings.
    pub sandbox: SandboxConfig,
    /// Concurrency limits.
    pub runtime: RuntimeConfig,
    /// Prices in USD per million tokens, keyed by `provider/model`.
    pub pricing: BTreeMap<String, Price>,
    /// Interface settings.
    pub ui: UiConfig,
}

/// Provider settings.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Providers {
    /// Anthropic Messages API.
    pub anthropic: AnthropicConfig,
    /// OpenAI Responses API.
    pub openai: OpenAiConfig,
}

/// What to do when Claude reports a preserved-thinking prefix mismatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MismatchBehavior {
    /// Fail the request (use in tests and CI so history edits are caught).
    Error,
    /// Drop the affected thinking blocks and continue (production).
    DropBlock,
}

/// Server-side refusal fallback setting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServerFallbacks {
    /// Let the provider route refusals to its default fallback models.
    Default,
    /// No server-side fallback.
    Off,
}

/// Anthropic provider settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AnthropicConfig {
    /// Where the API key lives.
    pub api_key: Option<SecretRef>,
    /// `anthropic-version` header value.
    pub api_version: String,
    /// Refusal fallback routing.
    pub server_fallbacks: ServerFallbacks,
    /// Preserved-thinking mismatch handling.
    pub preserved_thinking_mismatch: MismatchBehavior,
    /// Extra opt-in beta names.
    pub betas: Vec<String>,
}

impl Default for AnthropicConfig {
    fn default() -> Self {
        Self {
            api_key: Some(SecretRef::Keyring {
                name: "anthropic".into(),
            }),
            api_version: "2026-10-21".into(),
            server_fallbacks: ServerFallbacks::Default,
            preserved_thinking_mismatch: MismatchBehavior::DropBlock,
            betas: Vec::new(),
        }
    }
}

/// OpenAI provider settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OpenAiConfig {
    /// Where the API key lives.
    pub api_key: Option<SecretRef>,
    /// Whether the provider stores responses (the harness owns history, so `false`).
    pub store: bool,
}

impl Default for OpenAiConfig {
    fn default() -> Self {
        Self {
            api_key: Some(SecretRef::Keyring {
                name: "openai".into(),
            }),
            store: false,
        }
    }
}

/// Context engine settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct ContextConfig {
    /// Compact when input reaches this share of the context window (0 < r <= 1).
    pub compact_at_ratio: f64,
    /// Compact at this many input tokens at the latest.
    pub compact_at_tokens: u64,
    /// Tool output cap when recorded.
    pub output_cap_tokens: u64,
    /// Model for compaction: `same` or a `provider/model`.
    pub compaction_model: String,
    /// Harness-side auto-recall (P2, off until evaluated).
    pub auto_recall: bool,
    /// Token budget for auto-recalled snippets.
    pub auto_recall_budget_tokens: u64,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            compact_at_ratio: 0.70,
            compact_at_tokens: 250_000,
            output_cap_tokens: 2_000,
            compaction_model: "same".into(),
            auto_recall: false,
            auto_recall_budget_tokens: 800,
        }
    }
}

/// Named pairing of sandbox mode and approval policy (Tech spec 3.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PolicyProfile {
    /// Read freely; ask before changes.
    ReadOnly,
    /// Edit and run inside the project; ask to go further.
    Auto,
    /// Start in plan mode: read only until the user approves a plan.
    Plan,
    /// Never ask; deny what would need approval.
    Ci,
    /// No walls, no questions.
    Full,
}

/// Permission settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct PolicyConfig {
    /// Active profile.
    pub profile: PolicyProfile,
    /// Glob patterns that are always denied to file tools.
    pub default_deny_paths: Vec<String>,
}

impl Default for PolicyConfig {
    fn default() -> Self {
        Self {
            profile: PolicyProfile::Auto,
            default_deny_paths: ["**/.env*", "**/*.pem", "~/.ssh/**", "**/.harness/**"]
                .map(String::from)
                .to_vec(),
        }
    }
}

/// Sandbox backend choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SandboxBackend {
    /// Pick the best available.
    Auto,
    /// bubblewrap.
    Bwrap,
    /// Linux Landlock.
    Landlock,
    /// No sandbox.
    None,
}

/// Sandbox settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SandboxConfig {
    /// Backend.
    pub backend: SandboxBackend,
    /// Network access inside the sandbox.
    pub network: bool,
}

impl Default for SandboxConfig {
    fn default() -> Self {
        Self {
            backend: SandboxBackend::Auto,
            network: false,
        }
    }
}

/// Concurrency limits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RuntimeConfig {
    /// Parallel read-only tool calls per turn.
    pub max_parallel_tools: u32,
    /// Parallel subagents (global).
    pub max_parallel_agents: u32,
    /// Maximum subagent nesting depth.
    pub max_agent_depth: u32,
    /// Capacity of each session's op channel.
    pub op_channel_capacity: u32,
}

impl Default for RuntimeConfig {
    fn default() -> Self {
        Self {
            max_parallel_tools: 4,
            max_parallel_agents: 3,
            max_agent_depth: 2,
            op_channel_capacity: 64,
        }
    }
}

/// Price of a model in USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Price {
    /// Uncached input.
    pub input: f64,
    /// Output.
    pub output: f64,
    /// Cache reads (defaults to `input` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<f64>,
    /// Cache writes (defaults to `input` when absent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<f64>,
}

/// Colour theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    /// Follow the terminal.
    Auto,
    /// Light.
    Light,
    /// Dark.
    Dark,
}

/// Interface settings.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct UiConfig {
    /// Show reasoning summaries.
    pub show_thinking: bool,
    /// Colour theme.
    pub theme: Theme,
    /// The TUI captures the mouse, so the wheel scrolls the transcript.
    /// Selecting text then needs Shift+drag in most terminals; `false` gives
    /// the mouse back to the terminal.
    pub mouse: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            show_thinking: false,
            theme: Theme::Auto,
            mouse: true,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        let price = |input, output, cache_read| Price {
            input,
            output,
            cache_read,
            cache_write: None,
        };
        Self {
            default_persona: "default".into(),
            default_model: "anthropic/claude-opus-5-5"
                .parse()
                .expect("the built-in default model is valid"),
            providers: Providers::default(),
            context: ContextConfig::default(),
            policy: PolicyConfig::default(),
            sandbox: SandboxConfig::default(),
            runtime: RuntimeConfig::default(),
            pricing: BTreeMap::from([
                (
                    "anthropic/claude-opus-5-5".into(),
                    price(4.00, 20.00, Some(0.20)),
                ),
                (
                    "anthropic/claude-sonnet-5-5".into(),
                    price(2.00, 10.00, Some(0.10)),
                ),
                ("anthropic/claude-haiku-5-5".into(), price(0.10, 0.50, None)),
                // OpenAI Standard tier, prompts up to 272K input tokens (longer ones
                // cost 2x input and 1.5x output, which this table does not model). From
                // https://developers.openai.com/api/docs/pricing, read 2026-11-18.
                // Unlisted cache-write prices fall back to the input price.
                (
                    "openai/gpt-6-luna".into(),
                    Price {
                        cache_write: Some(0.125),
                        ..price(0.10, 0.50, Some(0.01))
                    },
                ),
                ("openai/gpt-6-sol".into(), price(2.00, 10.00, Some(0.20))),
                ("openai/gpt-6.1-sol".into(), price(2.00, 10.00, Some(0.10))),
                ("openai/gpt-6.1-terra".into(), price(1.00, 5.00, Some(0.05))),
                ("openai/gpt-6.2-sol".into(), price(2.00, 10.00, Some(0.10))),
                ("openai/gpt-6-astra".into(), price(10.00, 50.00, Some(1.00))),
                (
                    "openai/gpt-6.2-astra".into(),
                    price(8.00, 40.00, Some(0.40)),
                ),
            ]),
            ui: UiConfig::default(),
        }
    }
}

impl Config {
    /// Checks values that the type system cannot express.
    ///
    /// # Errors
    /// Returns [`ConfigError::Invalid`] naming the first bad key.
    pub fn validate(&self) -> Result<(), ConfigError> {
        let invalid = |msg: String| Err(ConfigError::Invalid(msg));
        if self.default_persona.trim().is_empty() {
            return invalid("default_persona must not be empty".into());
        }
        if self.providers.anthropic.api_version.trim().is_empty() {
            return invalid("providers.anthropic.api_version must not be empty".into());
        }
        // Written so NaN fails too.
        let ratio = self.context.compact_at_ratio;
        if !(ratio > 0.0 && ratio <= 1.0) {
            return invalid(format!(
                "context.compact_at_ratio must be > 0 and <= 1 (got {ratio})"
            ));
        }
        let positive = [
            ("context.compact_at_tokens", self.context.compact_at_tokens),
            ("context.output_cap_tokens", self.context.output_cap_tokens),
            (
                "runtime.max_parallel_tools",
                u64::from(self.runtime.max_parallel_tools),
            ),
            (
                "runtime.max_parallel_agents",
                u64::from(self.runtime.max_parallel_agents),
            ),
            (
                "runtime.op_channel_capacity",
                u64::from(self.runtime.op_channel_capacity),
            ),
        ];
        if let Some((key, _)) = positive.iter().find(|(_, v)| *v == 0) {
            return invalid(format!("{key} must be greater than 0"));
        }
        let model = &self.context.compaction_model;
        if model != "same" && model.parse::<ModelRef>().is_err() {
            return invalid(format!(
                "context.compaction_model must be \"same\" or \"provider/model\" (got {model:?})"
            ));
        }
        for (key, price) in &self.pricing {
            if key.parse::<ModelRef>().is_err() {
                return invalid(format!("pricing key {key:?} must be \"provider/model\""));
            }
            let values = [
                Some(price.input),
                Some(price.output),
                price.cache_read,
                price.cache_write,
            ];
            if values
                .into_iter()
                .flatten()
                .any(|v| !v.is_finite() || v < 0.0)
            {
                return invalid(format!("pricing.{key:?} prices must be finite and >= 0"));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn defaults_match_the_spec() {
        let c = Config::default();
        assert_eq!(c.default_persona, "default");
        assert_eq!(c.providers.anthropic.api_version, "2026-10-21");
        assert_eq!(
            c.providers.anthropic.preserved_thinking_mismatch,
            MismatchBehavior::DropBlock
        );
        assert!(!c.providers.openai.store);
        assert_eq!(c.context.compact_at_tokens, 250_000);
        assert_eq!(c.context.output_cap_tokens, 2_000);
        assert!(!c.context.auto_recall);
        assert_eq!(c.policy.profile, PolicyProfile::Auto);
        assert!(c.policy.default_deny_paths.iter().any(|p| p == "**/.env*"));
        assert!(!c.sandbox.network);
    }

    #[test]
    fn default_model_defaults_to_claude_and_must_be_a_model_ref() {
        assert_eq!(
            Config::default().default_model.to_string(),
            "anthropic/claude-opus-5-5"
        );
        let c: Config = toml::from_str("default_model = \"openai/gpt-5.5\"").unwrap();
        assert_eq!(c.default_model.provider(), "openai");
        assert!(toml::from_str::<Config>("default_model = \"gpt-5.5\"").is_err());
    }

    #[test]
    fn delegate_settings_no_longer_exist() {
        // ADR-026: harness never hands tasks to external agent CLIs.
        assert!(toml::from_str::<Config>("[providers.delegate]\nenabled = true").is_err());
    }

    #[test]
    fn openai_models_have_default_prices() {
        let luna = &Config::default().pricing["openai/gpt-6-luna"];
        assert_eq!(
            (luna.input, luna.output, luna.cache_read, luna.cache_write),
            (0.10, 0.50, Some(0.01), Some(0.125))
        );
        assert!(Config::default().validate().is_ok());
    }

    #[test]
    fn mouse_capture_is_on_unless_switched_off() {
        assert!(Config::default().ui.mouse);
        let config: Config = toml::from_str("[ui]\nmouse = false").unwrap();
        assert!(!config.ui.mouse);
    }

    #[test]
    fn empty_toml_gives_defaults() {
        let c: Config = toml::from_str("").unwrap();
        assert_eq!(c, Config::default());
    }

    #[test]
    fn unknown_keys_are_rejected() {
        let err = toml::from_str::<Config>("[context]\ncompact_at_tokenz = 1").unwrap_err();
        assert!(err.to_string().contains("compact_at_tokenz"), "{err}");
    }

    #[test]
    fn plain_secret_in_api_key_is_rejected() {
        assert!(toml::from_str::<Config>("[providers.anthropic]\napi_key = \"sk-ant-x\"").is_err());
    }

    #[test]
    fn ratio_must_be_in_zero_one() {
        for bad in [0.0, -0.1, 1.5, f64::NAN] {
            let mut c = Config::default();
            c.context.compact_at_ratio = bad;
            assert!(c.validate().is_err(), "{bad}");
        }
        let mut c = Config::default();
        c.context.compact_at_ratio = 1.0;
        c.validate().unwrap();
    }

    #[test]
    fn zero_limits_are_invalid() {
        let mut c = Config::default();
        c.runtime.op_channel_capacity = 0;
        let err = c.validate().unwrap_err();
        assert!(
            err.to_string().contains("runtime.op_channel_capacity"),
            "{err}"
        );
        let mut c = Config::default();
        c.context.output_cap_tokens = 0;
        assert!(c.validate().is_err());
    }

    #[test]
    fn pricing_keys_must_be_model_refs_and_prices_non_negative() {
        let mut c = Config::default();
        c.pricing.insert(
            "not-a-model".into(),
            Price {
                input: 1.0,
                output: 1.0,
                cache_read: None,
                cache_write: None,
            },
        );
        assert!(
            c.validate()
                .unwrap_err()
                .to_string()
                .contains("not-a-model")
        );
        let mut c = Config::default();
        c.pricing.insert(
            "x/y".into(),
            Price {
                input: -1.0,
                output: 1.0,
                cache_read: None,
                cache_write: None,
            },
        );
        assert!(c.validate().is_err());
    }

    #[test]
    fn compaction_model_is_same_or_a_model_ref() {
        let mut c = Config::default();
        c.context.compaction_model = "anthropic/claude-sonnet-5-5".into();
        c.validate().unwrap();
        c.context.compaction_model = "sonnet".into();
        assert!(c.validate().is_err());
    }
}
