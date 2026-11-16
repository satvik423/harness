//! [`ConfigError`]: everything that can go wrong while loading configuration.

use std::path::PathBuf;

use thiserror::Error;

/// Errors from loading, validating or saving configuration.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConfigError {
    /// The home directory could not be determined.
    #[error("cannot determine the home directory; set HOME")]
    NoHome,
    /// A file could not be read or written.
    #[error("cannot access {path}: {message}")]
    Io {
        /// The file.
        path: PathBuf,
        /// The OS error.
        message: String,
    },
    /// A file is not valid TOML, or does not match the schema.
    #[error("{source_name}: {message}")]
    Parse {
        /// Where the bad input came from (a file path, `environment`, `cli`).
        source_name: String,
        /// What is wrong, including the offending key where known.
        message: String,
    },
    /// The merged configuration is well-formed but has an invalid value.
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

impl ConfigError {
    pub(crate) fn io(path: impl Into<PathBuf>, err: &std::io::Error) -> Self {
        Self::Io {
            path: path.into(),
            message: err.to_string(),
        }
    }
}
