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
use harness_types::ModelRef;

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

    /// The profile name shown in the TUI header.
    #[must_use]
    pub fn profile_name(&self) -> &'static str {
        if self.full_access {
            return "full";
        }
        match self.config.policy.profile {
            PolicyProfile::ReadOnly => "read-only",
            PolicyProfile::Auto => "auto",
            PolicyProfile::Ci => "ci",
            PolicyProfile::Full => "full",
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
    fn models_start_with_the_default_and_profiles_have_names() {
        let dir = tempfile::tempdir().unwrap();
        let mut f = factory(dir.path());
        let models = f.models();
        assert_eq!(models[0], f.config.default_model);
        assert_eq!(
            models
                .iter()
                .filter(|m| **m == f.config.default_model)
                .count(),
            1
        );
        assert_eq!(f.profile_name(), "auto");
        f.full_access = true;
        assert_eq!(f.profile_name(), "full");
    }
}
