//! Layered configuration for the harness (SDD 2.10, Tech spec 3.17).
//!
//! Layers merge from lowest to highest precedence:
//!
//! 1. built-in defaults ([`Config::default`])
//! 2. `~/.config/harness/config.toml`
//! 3. `<project>/.harness/config.toml`, **only if the project is trusted** ([`TrustStore`])
//! 4. the resolved persona
//! 5. `HARNESS_*` environment variables ([`env_layer`])
//! 6. command-line flags
//!
//! Tables merge key by key; other values (including arrays) are replaced
//! ([`deep_merge`]). Credentials are never stored in config: secret fields hold
//! a [`SecretRef`] such as `keyring:anthropic`.
//!
//! # Example
//! ```
//! use harness_config::{ConfigLoader, Paths, PolicyProfile};
//! let home = tempfile::tempdir().unwrap();
//! let paths = Paths {
//!     user_config_dir: home.path().join("config"),
//!     user_data_dir: home.path().join("data"),
//! };
//! let loaded = ConfigLoader::new(paths).load().unwrap();
//! assert_eq!(loaded.config.policy.profile, PolicyProfile::Auto);
//! ```
#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod env;
mod error;
mod loader;
mod merge;
mod paths;
mod schema;
mod secret;
mod trust;

pub use env::env_layer;
pub use error::ConfigError;
pub use loader::{ConfigLoader, Layer, LoadedConfig};
pub use merge::deep_merge;
pub use paths::Paths;
pub use schema::{
    AnthropicConfig, Config, ContextConfig, DelegateConfig, MismatchBehavior, OpenAiConfig,
    PolicyConfig, PolicyProfile, Price, Providers, RuntimeConfig, SandboxBackend, SandboxConfig,
    ServerFallbacks, Theme, UiConfig,
};
pub use secret::{InvalidSecretRef, SecretRef};
pub use trust::TrustStore;
