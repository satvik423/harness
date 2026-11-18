//! Builds the adapters for a run: the provider for the chosen model and the
//! policy engine.

use std::path::Path;
use std::sync::Arc;

use harness_config::{Config, Paths, PolicyConfig, PolicyProfile};
use harness_policy::{ApprovalPolicy, PolicyEngine, PolicySettings, Profile, RuleError};
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
    /// The `full` profile was configured without `--dangerously-full-access`.
    #[error(
        "policy.profile is \"full\", which removes every safeguard; pass --dangerously-full-access to confirm, or choose another profile"
    )]
    FullAccessNotConfirmed,
    /// A rules file or default deny path is invalid.
    #[error("invalid policy rules: {0}")]
    Rules(#[from] RuleError),
}

/// What the policy engine is built from.
#[derive(Debug, Clone)]
pub struct PolicyInputs<'a> {
    /// The `[policy]` config section.
    pub config: &'a PolicyConfig,
    /// User-level directories (for the user's `rules.toml`).
    pub paths: &'a Paths,
    /// The project directory.
    pub project: &'a Path,
    /// Whether the project is trusted (its `rules.toml` is used only then).
    pub trusted: bool,
    /// `--approval`, overriding the profile's approval policy.
    pub approval: Option<ApprovalPolicy>,
    /// `--dangerously-full-access`: run with the `full` profile.
    pub full_access: bool,
    /// Whether commands run in an isolating sandbox.
    pub sandboxed: bool,
    /// The home directory, for `~/` in rules.
    pub home: Option<&'a Path>,
}

/// Builds the policy engine for a run.
///
/// # Errors
/// [`WiringError::FullAccessNotConfirmed`] if the config selects the `full`
/// profile without the flag; [`WiringError::Rules`] for invalid rules.
pub fn build_policy(inputs: &PolicyInputs<'_>) -> Result<Arc<PolicyEngine>, WiringError> {
    let profile = match (inputs.full_access, inputs.config.profile) {
        (true, _) => Profile::Full,
        (false, PolicyProfile::Full) => return Err(WiringError::FullAccessNotConfirmed),
        (false, PolicyProfile::ReadOnly) => Profile::ReadOnly,
        (false, PolicyProfile::Auto) => Profile::Auto,
        (false, PolicyProfile::Ci) => Profile::Ci,
    };
    let engine = PolicyEngine::new(PolicySettings {
        profile,
        approval: inputs.approval.unwrap_or(profile.default_approval()),
        sandboxed: inputs.sandboxed,
        default_deny_paths: inputs.config.default_deny_paths.clone(),
        home: inputs.home.map(Path::to_path_buf),
        user_rules: Some(inputs.paths.user_rules_file()),
        project_rules: inputs
            .trusted
            .then(|| Paths::project_rules_file(inputs.project)),
    })?;
    Ok(Arc::new(engine))
}

/// The model a stored session's current segment was opened with, read from
/// its frozen frame (ADR-032). `None` if the session has no segment yet.
///
/// # Errors
/// A store error, or [`harness_ports::StoreError::Corrupt`] for a bad frame.
pub async fn session_model(
    log: &dyn harness_ports::EventLog,
    blobs: &dyn harness_ports::BlobStore,
    session: harness_types::SessionId,
) -> Result<Option<ModelRef>, harness_ports::StoreError> {
    let events = log
        .range(
            session,
            harness_types::Seq::FIRST,
            harness_types::Seq(u64::MAX),
        )
        .await?;
    let Some(frame) = events.iter().rev().find_map(|e| match &e.envelope.event {
        harness_protocol::Event::SegmentOpened { frame, .. } => Some(*frame),
        _ => None,
    }) else {
        return Ok(None);
    };
    let bytes = blobs.get(&frame).await?;
    let frame: harness_context::SegmentFrame = serde_json::from_slice(&bytes)
        .map_err(|e| harness_ports::StoreError::Corrupt(format!("segment frame: {e}")))?;
    format!("{}/{}", frame.provider, frame.model)
        .parse()
        .map(Some)
        .map_err(|e| harness_ports::StoreError::Corrupt(format!("segment frame model: {e}")))
}

/// A provider for commands that never call a model (`session undo`,
/// `task cancel`): every request fails with a clear message.
#[derive(Debug, Clone, Copy, Default)]
pub struct Offline;

#[async_trait::async_trait]
impl Provider for Offline {
    fn id(&self) -> &str {
        "offline"
    }

    async fn capabilities(
        &self,
        _: &str,
    ) -> Result<harness_ports::Capabilities, harness_ports::ProviderError> {
        Err(harness_ports::ProviderError::Unsupported(
            "this command does not call a model".into(),
        ))
    }

    async fn stream(
        &self,
        _: harness_ports::ModelRequest,
        _: tokio_util::sync::CancellationToken,
    ) -> Result<harness_ports::ProviderStream, harness_ports::ProviderError> {
        Err(harness_ports::ProviderError::Unsupported(
            "this command does not call a model".into(),
        ))
    }

    async fn count_tokens(
        &self,
        _: &harness_ports::ModelRequest,
    ) -> Result<u64, harness_ports::ProviderError> {
        Ok(0)
    }
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
    use harness_ports::{Policy, PolicyCall, Verdict};
    use harness_types::Risk;

    struct PolicyFixture {
        dir: tempfile::TempDir,
        paths: Paths,
        config: PolicyConfig,
    }

    impl PolicyFixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let paths = Paths {
                user_config_dir: dir.path().join("config"),
                user_data_dir: dir.path().join("data"),
            };
            std::fs::create_dir_all(dir.path().join("proj/.harness")).unwrap();
            Self {
                dir,
                paths,
                config: PolicyConfig::default(),
            }
        }

        fn project(&self) -> std::path::PathBuf {
            self.dir.path().join("proj")
        }

        fn inputs(&self, trusted: bool) -> PolicyInputs<'_> {
            PolicyInputs {
                config: &self.config,
                paths: &self.paths,
                project: self.dir.path().join("proj").leak(),
                trusted,
                approval: None,
                full_access: false,
                sandboxed: false,
                home: None,
            }
        }

        fn shell(&self, command: &str) -> PolicyCall {
            PolicyCall {
                tool: "shell".into(),
                input: serde_json::json!({}),
                risk: Risk::Exec {
                    command: command.into(),
                },
                cwd: self.project(),
                writable_roots: vec![self.project()],
            }
        }
    }

    #[test]
    fn full_profile_needs_the_flag() {
        let mut f = PolicyFixture::new();
        f.config.profile = PolicyProfile::Full;
        assert!(matches!(
            build_policy(&f.inputs(false)),
            Err(WiringError::FullAccessNotConfirmed)
        ));
        let mut inputs = f.inputs(false);
        inputs.full_access = true;
        let engine = build_policy(&inputs).unwrap();
        assert_eq!(engine.settings().profile, Profile::Full);
    }

    #[test]
    fn the_approval_flag_overrides_the_profile() {
        let f = PolicyFixture::new();
        let engine = build_policy(&f.inputs(false)).unwrap();
        assert_eq!(engine.settings().approval, ApprovalPolicy::OnRequest);
        let mut inputs = f.inputs(false);
        inputs.approval = Some(ApprovalPolicy::Never);
        assert_eq!(
            build_policy(&inputs).unwrap().settings().approval,
            ApprovalPolicy::Never
        );
    }

    #[test]
    fn project_rules_apply_only_to_trusted_projects() {
        let f = PolicyFixture::new();
        std::fs::write(
            f.project().join(".harness/rules.toml"),
            "[[rule]]\ntool = \"shell\"\nmatch = \"prefix:make\"\ndecision = \"allow\"",
        )
        .unwrap();
        let untrusted = build_policy(&f.inputs(false)).unwrap();
        assert!(matches!(
            untrusted.evaluate(&f.shell("make")),
            Verdict::Ask { .. }
        ));
        let trusted = build_policy(&f.inputs(true)).unwrap();
        assert!(matches!(
            trusted.evaluate(&f.shell("make")),
            Verdict::Allow { .. }
        ));
    }

    #[test]
    fn user_rules_are_loaded_and_invalid_ones_are_reported() {
        let f = PolicyFixture::new();
        std::fs::create_dir_all(&f.paths.user_config_dir).unwrap();
        std::fs::write(
            f.paths.user_rules_file(),
            "[[rule]]\ntool = \"shell\"\ndecision = \"sometimes\"",
        )
        .unwrap();
        let err = build_policy(&f.inputs(false)).unwrap_err();
        assert!(err.to_string().contains("rules.toml"), "{err}");
    }
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
