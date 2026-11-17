//! Builds the adapters for a run: the provider for the chosen model.

use std::sync::Arc;

use harness_config::Config;
use harness_ports::Provider;
use harness_types::ModelRef;
use thiserror::Error;

use harness_provider_anthropic::AnthropicProvider;
use harness_provider_openai::OpenAiProvider;

use crate::run::{anthropic_settings, openai_settings};
use crate::secrets::{KeyFor, SecretError, SecretSource, resolve_key};

/// Errors from building the provider.
#[derive(Debug, Error)]
pub enum WiringError {
    /// The model names a provider harness does not support.
    #[error("unknown provider {0:?} in the model name; supported providers: anthropic, openai")]
    UnknownProvider(String),
    /// The provider's key could not be found.
    #[error(transparent)]
    Secret(#[from] SecretError),
    /// The provider could not be created.
    #[error("cannot create the {provider} provider: {message}")]
    Provider {
        /// Provider id.
        provider: String,
        /// What went wrong.
        message: String,
    },
}

/// Creates the provider for `model`, resolving its API key from `secrets`.
///
/// # Errors
/// [`WiringError`] for an unknown provider, a missing key or a client failure.
pub fn build_provider(
    model: &ModelRef,
    config: &Config,
    secrets: &dyn SecretSource,
) -> Result<Arc<dyn Provider>, WiringError> {
    let failed = |e: harness_ports::ProviderError| WiringError::Provider {
        provider: model.provider().to_owned(),
        message: e.to_string(),
    };
    let providers = &config.providers;
    match model.provider() {
        "anthropic" => {
            let key = resolve_key(
                KeyFor::Anthropic,
                providers.anthropic.api_key.as_ref(),
                secrets,
            )?;
            let provider = AnthropicProvider::new(anthropic_settings(&providers.anthropic), key)
                .map_err(failed)?;
            Ok(Arc::new(provider))
        }
        "openai" => {
            let key = resolve_key(KeyFor::OpenAi, providers.openai.api_key.as_ref(), secrets)?;
            let provider =
                OpenAiProvider::new(openai_settings(&providers.openai), key).map_err(failed)?;
            Ok(Arc::new(provider))
        }
        other => Err(WiringError::UnknownProvider(other.to_owned())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::tests::Fake;
    use std::collections::HashMap;

    fn with_keys() -> Fake {
        Fake {
            keyring: HashMap::from([
                ("anthropic".into(), "sk-a".into()),
                ("openai".into(), "sk-o".into()),
            ]),
            ..Fake::default()
        }
    }

    #[test]
    fn picks_the_adapter_from_the_model_provider() {
        let config = Config::default();
        let anthropic = build_provider(
            &"anthropic/claude-opus-5-5".parse().unwrap(),
            &config,
            &with_keys(),
        )
        .unwrap();
        assert_eq!(anthropic.id(), "anthropic");
        let openai =
            build_provider(&"openai/gpt-x".parse().unwrap(), &config, &with_keys()).unwrap();
        assert_eq!(openai.id(), "openai");
    }

    #[test]
    fn unknown_providers_are_rejected() {
        let err = build_provider(
            &"google/gemini".parse().unwrap(),
            &Config::default(),
            &with_keys(),
        )
        .err()
        .unwrap();
        assert!(matches!(err, WiringError::UnknownProvider(p) if p == "google"));
    }

    #[test]
    fn a_missing_key_names_the_right_provider() {
        let err = build_provider(
            &"openai/gpt-x".parse().unwrap(),
            &Config::default(),
            &Fake::default(),
        )
        .err()
        .unwrap();
        assert!(err.to_string().contains("no OpenAI API key"), "{err}");
    }
}
