//! [`SecretRef`]: how config refers to a credential without containing it.

use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Error returned when a secret reference is malformed or is a plain secret.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error(
    "secret values must be references like \"keyring:NAME\" or \"env:VAR\", never the secret itself (got {0})"
)]
pub struct InvalidSecretRef(pub String);

/// A reference to a credential stored outside the config file (NFR-08).
///
/// Config files never hold secrets; they hold one of these. Resolution
/// (reading the OS keyring or the environment) happens in the provider layer.
///
/// # Examples
/// ```
/// use harness_config::SecretRef;
/// let r: SecretRef = "keyring:anthropic".parse().unwrap();
/// assert_eq!(r, SecretRef::Keyring { name: "anthropic".into() });
/// assert!("sk-ant-api03-…".parse::<SecretRef>().is_err());
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SecretRef {
    /// An entry in the OS keyring under the `harness` service.
    Keyring {
        /// Entry name, for example `anthropic`.
        name: String,
    },
    /// An environment variable.
    Env {
        /// Variable name, for example `ANTHROPIC_API_KEY`.
        var: String,
    },
}

impl fmt::Display for SecretRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Keyring { name } => write!(f, "keyring:{name}"),
            Self::Env { var } => write!(f, "env:{var}"),
        }
    }
}

impl FromStr for SecretRef {
    type Err = InvalidSecretRef;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Never put the raw input in the error: it may be a pasted API key.
        let Some((scheme, rest)) = s.split_once(':') else {
            return Err(InvalidSecretRef(format!(
                "a plain value of {} characters",
                s.len()
            )));
        };
        match scheme {
            "keyring"
                if !rest.is_empty()
                    && !rest.chars().any(|c| c.is_whitespace() || c.is_control()) =>
            {
                Ok(Self::Keyring {
                    name: rest.to_owned(),
                })
            }
            "keyring" => Err(InvalidSecretRef(
                "an empty or malformed keyring name".into(),
            )),
            "env" if is_env_var_name(rest) => Ok(Self::Env {
                var: rest.to_owned(),
            }),
            "env" => Err(InvalidSecretRef(
                "an invalid environment variable name".into(),
            )),
            _ => Err(InvalidSecretRef(format!(
                "a value of {} characters with no known scheme",
                s.len()
            ))),
        }
    }
}

/// `true` for names made of ASCII letters, digits and `_`, not starting with a digit.
fn is_env_var_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    matches!(bytes.next(), Some(b) if b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

impl Serialize for SecretRef {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for SecretRef {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keyring_and_env_references() {
        assert_eq!(
            "keyring:openai".parse(),
            Ok(SecretRef::Keyring {
                name: "openai".into()
            })
        );
        assert_eq!(
            "env:OPENAI_API_KEY".parse(),
            Ok(SecretRef::Env {
                var: "OPENAI_API_KEY".into()
            })
        );
    }

    #[test]
    fn rejects_plain_values_without_echoing_them() {
        let err = "sk-ant-api03-XYZZY".parse::<SecretRef>().unwrap_err();
        assert!(
            !err.to_string().contains("XYZZY"),
            "error must not echo the secret: {err}"
        );
    }

    #[test]
    fn rejects_empty_names_and_unknown_schemes() {
        for bad in ["keyring:", "env:", "vault:x", ""] {
            assert!(bad.parse::<SecretRef>().is_err(), "{bad}");
        }
    }

    #[test]
    fn env_var_names_must_be_shell_safe() {
        assert!("env:HAS SPACE".parse::<SecretRef>().is_err());
        assert!("env:lower_ok_2".parse::<SecretRef>().is_ok());
    }

    #[test]
    fn display_round_trips() {
        for s in ["keyring:anthropic", "env:X"] {
            assert_eq!(s.parse::<SecretRef>().unwrap().to_string(), s);
        }
    }
}
