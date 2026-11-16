//! [`ModelRef`]: a provider-qualified model name such as `anthropic/claude-opus-5-5`.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned when a [`ModelRef`] fails to parse.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseModelRefError {
    /// There was no `/` separating provider and model.
    #[error("model reference {0:?} must look like \"provider/model\"")]
    MissingSlash(String),
    /// The provider part was empty or contained characters other than `a-z`, `0-9`, `-`.
    #[error("invalid provider {0:?}: use lowercase letters, digits and '-'")]
    InvalidProvider(String),
    /// The model part was empty or contained whitespace.
    #[error("invalid model name {0:?}: must be non-empty with no whitespace")]
    InvalidModel(String),
}

/// A model identified by provider and provider-specific model id.
///
/// Personas and config name models this way; the provider router uses
/// [`ModelRef::provider`] to pick an adapter and passes [`ModelRef::model`] on the wire.
///
/// # Examples
/// ```
/// use harness_types::ModelRef;
/// let m: ModelRef = "anthropic/claude-opus-5-5".parse().unwrap();
/// assert_eq!(m.provider(), "anthropic");
/// assert_eq!(m.model(), "claude-opus-5-5");
/// assert_eq!(m.to_string(), "anthropic/claude-opus-5-5");
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ModelRef {
    provider: String,
    model: String,
}

impl ModelRef {
    /// Returns the provider id, for example `anthropic`.
    #[must_use]
    pub fn provider(&self) -> &str {
        &self.provider
    }

    /// Returns the provider's model id, for example `claude-opus-5-5`.
    #[must_use]
    pub fn model(&self) -> &str {
        &self.model
    }
}

impl fmt::Display for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.provider, self.model)
    }
}

impl FromStr for ModelRef {
    type Err = ParseModelRefError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let (provider, model) = s
            .split_once('/')
            .ok_or_else(|| ParseModelRefError::MissingSlash(s.to_owned()))?;
        let provider_ok = !provider.is_empty()
            && provider
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-');
        if !provider_ok {
            return Err(ParseModelRefError::InvalidProvider(provider.to_owned()));
        }
        if model.is_empty() || model.chars().any(char::is_whitespace) {
            return Err(ParseModelRefError::InvalidModel(model.to_owned()));
        }
        Ok(Self {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }
}

impl Serialize for ModelRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for ModelRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_provider_and_model() {
        let m: ModelRef = "openai/gpt-5.5".parse().unwrap();
        assert_eq!((m.provider(), m.model()), ("openai", "gpt-5.5"));
    }

    #[test]
    fn model_may_contain_further_slashes() {
        let m: ModelRef = "openrouter/meta/llama-4".parse().unwrap();
        assert_eq!(m.model(), "meta/llama-4");
    }

    #[test]
    fn rejects_missing_slash() {
        assert!(matches!(
            "claude-opus-5-5".parse::<ModelRef>(),
            Err(ParseModelRefError::MissingSlash(_))
        ));
    }

    #[test]
    fn rejects_uppercase_or_empty_provider() {
        assert!(matches!(
            "Anthropic/x".parse::<ModelRef>(),
            Err(ParseModelRefError::InvalidProvider(_))
        ));
        assert!(matches!(
            "/x".parse::<ModelRef>(),
            Err(ParseModelRefError::InvalidProvider(_))
        ));
    }

    #[test]
    fn rejects_empty_or_whitespace_model() {
        assert!(matches!(
            "anthropic/".parse::<ModelRef>(),
            Err(ParseModelRefError::InvalidModel(_))
        ));
        assert!(matches!(
            "anthropic/a b".parse::<ModelRef>(),
            Err(ParseModelRefError::InvalidModel(_))
        ));
    }

    #[test]
    fn serde_uses_the_string_form() {
        let m: ModelRef = "anthropic/claude-haiku-5-5".parse().unwrap();
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(json, "\"anthropic/claude-haiku-5-5\"");
        assert_eq!(serde_json::from_str::<ModelRef>(&json).unwrap(), m);
        assert!(serde_json::from_str::<ModelRef>("\"nope\"").is_err());
    }
}
