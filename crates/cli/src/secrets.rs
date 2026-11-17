//! Resolves provider API keys from a [`SecretRef`] (ADR-008, NFR-08).
//!
//! Order: the configured reference (`keyring:NAME` or `env:VAR`); if a keyring
//! entry is missing or the keyring is unavailable, the provider's standard
//! environment variable. Keys are never logged or shown in errors.

use harness_config::SecretRef;
use harness_ports::ApiKey;
use thiserror::Error;

/// Keyring service under which harness stores credentials.
pub const KEYRING_SERVICE: &str = "harness";

/// Which provider a key is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyFor {
    /// Anthropic (`ANTHROPIC_API_KEY`, keyring entry `anthropic`).
    Anthropic,
    /// OpenAI (`OPENAI_API_KEY`, keyring entry `openai`).
    OpenAi,
}

impl KeyFor {
    /// Display name, for example `OpenAI`.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::OpenAi => "OpenAI",
        }
    }

    /// The provider's standard environment variable.
    #[must_use]
    pub fn env_var(self) -> &'static str {
        match self {
            Self::Anthropic => "ANTHROPIC_API_KEY",
            Self::OpenAi => "OPENAI_API_KEY",
        }
    }

    /// The command that stores this provider's key in the keyring.
    #[must_use]
    pub fn store_hint(self) -> String {
        let entry = match self {
            Self::Anthropic => "anthropic",
            Self::OpenAi => "openai",
        };
        format!(
            "secret-tool store --label='harness {entry}' service {KEYRING_SERVICE} username {entry}"
        )
    }
}

/// Where secrets can be read from (the OS in production, a fake in tests).
pub trait SecretSource {
    /// Reads keyring entry `name` under [`KEYRING_SERVICE`]; `Ok(None)` if absent.
    ///
    /// # Errors
    /// A description of why the keyring could not be used.
    fn keyring(&self, name: &str) -> Result<Option<String>, String>;

    /// Reads an environment variable.
    fn env(&self, var: &str) -> Option<String>;
}

/// Errors from resolving a key. Messages say how to fix the problem.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SecretError {
    /// No key was found anywhere.
    #[error(
        "no {provider} API key found (looked in {tried}). Store one with:\n  {hint}\nor set {env}."
    )]
    Missing {
        /// Provider name.
        provider: &'static str,
        /// The places that were checked.
        tried: String,
        /// The `secret-tool` command.
        hint: String,
        /// The fallback environment variable.
        env: &'static str,
    },
    /// The keyring failed and no fallback was available.
    #[error(
        "the keyring is not available ({reason}) and {env} is not set. Store the {provider} key with:\n  {hint}"
    )]
    Keyring {
        /// Provider name.
        provider: &'static str,
        /// Why the keyring failed.
        reason: String,
        /// The `secret-tool` command.
        hint: String,
        /// The fallback environment variable.
        env: &'static str,
    },
}

/// Finds the API key for `provider`.
///
/// # Errors
/// [`SecretError::Missing`] or [`SecretError::Keyring`], with instructions.
pub fn resolve_key(
    provider: KeyFor,
    reference: Option<&SecretRef>,
    source: &dyn SecretSource,
) -> Result<ApiKey, SecretError> {
    let clean =
        |value: Option<String>| value.map(|s| s.trim().to_owned()).filter(|s| !s.is_empty());
    let env_var = provider.env_var();
    let env_fallback = || clean(source.env(env_var));
    let missing = |tried: String| SecretError::Missing {
        provider: provider.label(),
        tried,
        hint: provider.store_hint(),
        env: env_var,
    };
    match reference {
        Some(SecretRef::Env { var }) => clean(source.env(var))
            .map(ApiKey::new)
            .ok_or_else(|| missing(format!("env:{var}"))),
        Some(SecretRef::Keyring { name }) => match source.keyring(name) {
            Ok(value) => clean(value)
                .or_else(env_fallback)
                .map(ApiKey::new)
                .ok_or_else(|| missing(format!("keyring:{name}, env:{env_var}"))),
            Err(reason) => env_fallback()
                .map(ApiKey::new)
                .ok_or_else(|| SecretError::Keyring {
                    provider: provider.label(),
                    reason,
                    hint: provider.store_hint(),
                    env: env_var,
                }),
        },
        None => env_fallback()
            .map(ApiKey::new)
            .ok_or_else(|| missing(format!("env:{env_var}"))),
    }
}

/// The real OS: the Secret Service keyring on Linux, and the process environment.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemSecrets;

impl SecretSource for SystemSecrets {
    #[cfg(target_os = "linux")]
    fn keyring(&self, name: &str) -> Result<Option<String>, String> {
        use std::sync::OnceLock;
        // Connect to the Secret Service once per process.
        static STORE: OnceLock<Result<(), String>> = OnceLock::new();
        STORE
            .get_or_init(|| {
                // `set_default_store` wants the trait object, so coerce explicitly.
                zbus_secret_service_keyring_store::Store::new()
                    .map(|store| keyring_core::set_default_store(store))
                    .map_err(|e| e.to_string())
            })
            .clone()?;
        let entry = keyring_core::Entry::new(KEYRING_SERVICE, name).map_err(|e| e.to_string())?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring_core::Error::NoEntry) => Ok(None),
            Err(e) => Err(e.to_string()),
        }
    }

    #[cfg(not(target_os = "linux"))]
    fn keyring(&self, _name: &str) -> Result<Option<String>, String> {
        Err("keyring support on this platform arrives in a later phase".into())
    }

    fn env(&self, var: &str) -> Option<String> {
        std::env::var(var).ok()
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::collections::HashMap;

    /// An in-memory [`SecretSource`].
    #[derive(Default)]
    pub(crate) struct Fake {
        pub(crate) keyring: HashMap<String, String>,
        pub(crate) keyring_error: Option<String>,
        pub(crate) env: HashMap<String, String>,
    }

    impl SecretSource for Fake {
        fn keyring(&self, name: &str) -> Result<Option<String>, String> {
            match &self.keyring_error {
                Some(e) => Err(e.clone()),
                None => Ok(self.keyring.get(name).cloned()),
            }
        }
        fn env(&self, var: &str) -> Option<String> {
            self.env.get(var).cloned()
        }
    }

    fn keyring_ref(name: &str) -> SecretRef {
        SecretRef::Keyring { name: name.into() }
    }

    #[test]
    fn keyring_entry_is_used_and_trimmed() {
        let fake = Fake {
            keyring: HashMap::from([("anthropic".into(), " sk-1\n".into())]),
            ..Fake::default()
        };
        assert_eq!(
            resolve_key(KeyFor::Anthropic, Some(&keyring_ref("anthropic")), &fake).unwrap(),
            ApiKey::new("sk-1")
        );
    }

    #[test]
    fn missing_keyring_entry_falls_back_to_the_providers_env_var() {
        let fake = Fake {
            env: HashMap::from([("OPENAI_API_KEY".into(), "sk-env".into())]),
            ..Fake::default()
        };
        assert_eq!(
            resolve_key(KeyFor::OpenAi, Some(&keyring_ref("openai")), &fake).unwrap(),
            ApiKey::new("sk-env")
        );
        // The other provider's variable is not used.
        assert!(resolve_key(KeyFor::Anthropic, Some(&keyring_ref("anthropic")), &fake).is_err());
    }

    #[test]
    fn keyring_failure_falls_back_or_explains() {
        let broken = Fake {
            keyring_error: Some("no secret service".into()),
            ..Fake::default()
        };
        let err = resolve_key(KeyFor::OpenAi, Some(&keyring_ref("openai")), &broken).unwrap_err();
        assert!(
            matches!(&err, SecretError::Keyring { reason, .. } if reason == "no secret service")
        );
        assert!(err.to_string().contains("username openai"), "{err}");
        let with_env = Fake {
            env: HashMap::from([("OPENAI_API_KEY".into(), "sk".into())]),
            ..broken
        };
        assert!(resolve_key(KeyFor::OpenAi, Some(&keyring_ref("openai")), &with_env).is_ok());
    }

    #[test]
    fn env_reference_reads_only_that_variable() {
        let fake = Fake {
            env: HashMap::from([("MY_KEY".into(), "sk-mine".into())]),
            ..Fake::default()
        };
        let r = SecretRef::Env {
            var: "MY_KEY".into(),
        };
        assert_eq!(
            resolve_key(KeyFor::Anthropic, Some(&r), &fake).unwrap(),
            ApiKey::new("sk-mine")
        );
        let r = SecretRef::Env {
            var: "OTHER".into(),
        };
        assert!(matches!(
            resolve_key(KeyFor::Anthropic, Some(&r), &fake),
            Err(SecretError::Missing { .. })
        ));
    }

    #[test]
    fn no_reference_uses_the_env_var() {
        let fake = Fake {
            env: HashMap::from([("ANTHROPIC_API_KEY".into(), "sk-env".into())]),
            ..Fake::default()
        };
        assert_eq!(
            resolve_key(KeyFor::Anthropic, None, &fake).unwrap(),
            ApiKey::new("sk-env")
        );
    }

    #[test]
    fn blank_values_count_as_missing_and_the_message_says_where_we_looked() {
        let fake = Fake {
            keyring: HashMap::from([("openai".into(), "  ".into())]),
            env: HashMap::from([("OPENAI_API_KEY".into(), String::new())]),
            ..Fake::default()
        };
        let err = resolve_key(KeyFor::OpenAi, Some(&keyring_ref("openai")), &fake).unwrap_err();
        let text = err.to_string();
        for needle in [
            "no OpenAI API key",
            "keyring:openai",
            "OPENAI_API_KEY",
            "secret-tool store --label='harness openai'",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in {text}");
        }
    }
}
