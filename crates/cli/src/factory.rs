//! [`SessionFactory`]: everything needed to build a session's dependencies,
//! shared by `harness run` and the TUI (which builds new sessions for `/clear`
//! and `/model`).

use std::path::PathBuf;
use std::sync::Arc;

use harness_config::{Config, Paths, PolicyProfile};
use harness_policy::{ApprovalPolicy, PolicyEngine};
use harness_ports::{Provider, SystemClock};
use harness_runtime::{ModelPrice, RuntimeDeps};
use harness_store_sqlite::SqliteStore;
use harness_tools::{MemoryReadLedger, Unsandboxed};
use harness_types::{Mode, ModelRef};

use crate::wiring::{PolicyInputs, WiringError, build_policy};

/// The settings and stores sessions are built from.
pub struct SessionFactory {
    /// The merged configuration.
    pub config: Config,
    /// User-level directories.
    pub paths: Paths,
    /// The project directory.
    pub cwd: PathBuf,
    /// Whether the project is trusted.
    pub trusted: bool,
    /// `--approval`, overriding the profile.
    pub approval: Option<ApprovalPolicy>,
    /// `--dangerously-full-access`.
    pub full_access: bool,
    /// `--mode`: the mode to start in (a resumed session switches to it).
    pub mode: Option<Mode>,
    /// The project's store.
    pub store: Arc<SqliteStore>,
}

impl SessionFactory {
    /// Builds the policy engine.
    ///
    /// # Errors
    /// See [`build_policy`].
    pub fn policy(&self) -> Result<Arc<PolicyEngine>, WiringError> {
        let home = std::env::home_dir();
        build_policy(&PolicyInputs {
            config: &self.config.policy,
            paths: &self.paths,
            project: &self.cwd,
            trusted: self.trusted,
            approval: self.approval,
            full_access: self.full_access,
            sandboxed: false,
            home: home.as_deref(),
        })
    }

    /// The runtime dependencies for a session using `provider`, and the
    /// effective approval policy.
    ///
    /// # Errors
    /// A policy or tool registry error.
    pub fn deps(
        &self,
        provider: Arc<dyn Provider>,
    ) -> Result<(RuntimeDeps, ApprovalPolicy), WiringError> {
        let policy = self.policy()?;
        let approval = policy.settings().approval;
        let tools = harness_tools::builtin_registry().map_err(|e| WiringError::Provider {
            provider: "tools".into(),
            message: e.to_string(),
        })?;
        let deps = RuntimeDeps {
            provider,
            tools,
            clock: Arc::new(SystemClock),
            sandbox: Arc::new(Unsandboxed),
            blobs: Arc::clone(&self.store) as _,
            ledger: Arc::new(MemoryReadLedger::default()),
            policy,
            log: Arc::clone(&self.store) as _,
            sessions: Arc::clone(&self.store) as _,
            snapshots: Arc::clone(&self.store) as _,
        };
        Ok((deps, approval))
    }

    /// The price of `model` from the config's pricing table.
    #[must_use]
    pub fn price(&self, model: &ModelRef) -> Option<ModelPrice> {
        self.config
            .pricing
            .get(&model.to_string())
            .map(|p| ModelPrice {
                input: p.input,
                output: p.output,
                cache_read: p.cache_read.unwrap_or(p.input),
                cache_write: p.cache_write.unwrap_or(p.input),
            })
    }

    /// The mode a new session starts in: `--mode`, else `full` with
    /// `--dangerously-full-access`, else the configured profile's mode.
    ///
    /// # Errors
    /// [`WiringError::FullModeNotConfirmed`] for `--mode full` without the flag.
    pub fn start_mode(&self) -> Result<Mode, WiringError> {
        match (self.mode, self.full_access) {
            (Some(Mode::Full), false) => Err(WiringError::FullModeNotConfirmed),
            (Some(mode), _) => Ok(mode),
            (None, true) => Ok(Mode::Full),
            (None, false) => Ok(match self.config.policy.profile {
                PolicyProfile::ReadOnly => Mode::ReadOnly,
                PolicyProfile::Auto | PolicyProfile::Ci => Mode::Auto,
                PolicyProfile::Plan => Mode::Plan,
                PolicyProfile::Full => Mode::Full,
            }),
        }
    }

    /// Models offered by `/model`: the default, then every priced model.
    #[must_use]
    pub fn models(&self) -> Vec<ModelRef> {
        let mut models = vec![self.config.default_model.clone()];
        for key in self.config.pricing.keys() {
            if let Ok(model) = key.parse::<ModelRef>()
                && !models.contains(&model)
            {
                models.push(model);
            }
        }
        models
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn factory(dir: &std::path::Path) -> SessionFactory {
        SessionFactory {
            config: Config::default(),
            paths: Paths {
                user_config_dir: dir.join("c"),
                user_data_dir: dir.join("d"),
            },
            cwd: dir.to_path_buf(),
            trusted: false,
            approval: None,
            full_access: false,
            mode: None,
            store: Arc::new(SqliteStore::open(dir).unwrap()),
        }
    }

    #[test]
    fn prices_come_from_the_pricing_table_with_cache_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let f = factory(dir.path());
        let opus = f
            .price(&"anthropic/claude-opus-5-5".parse().unwrap())
            .unwrap();
        assert!((opus.input - 4.0).abs() < 1e-9 && (opus.cache_read - 0.2).abs() < 1e-9);
        assert!(
            (opus.cache_write - 4.0).abs() < 1e-9,
            "cache writes default to the input price"
        );
        assert!(
            f.price(&"openai/not-a-priced-model".parse().unwrap())
                .is_none()
        );
    }

    #[test]
    fn models_start_with_the_default() {
        let dir = tempfile::tempdir().unwrap();
        let f = factory(dir.path());
        let models = f.models();
        assert_eq!(models[0], f.config.default_model);
        assert_eq!(
            models
                .iter()
                .filter(|m| **m == f.config.default_model)
                .count(),
            1
        );
    }

    #[test]
    fn the_start_mode_comes_from_the_flag_then_full_access_then_the_profile() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = factory(dir.path());
        assert_eq!(f.start_mode().unwrap(), Mode::Auto);
        f.config.policy.profile = PolicyProfile::Plan;
        assert_eq!(f.start_mode().unwrap(), Mode::Plan);
        f.full_access = true;
        assert_eq!(f.start_mode().unwrap(), Mode::Full);
        f.mode = Some(Mode::ReadOnly);
        assert_eq!(f.start_mode().unwrap(), Mode::ReadOnly);
        f.full_access = false;
        f.mode = Some(Mode::Full);
        assert!(matches!(
            f.start_mode(),
            Err(WiringError::FullModeNotConfirmed)
        ));
    }
}
