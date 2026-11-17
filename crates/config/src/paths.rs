//! [`Paths`]: where the harness keeps user-level files (SDD 2.9).

use std::path::{Path, PathBuf};

use crate::ConfigError;

/// User-level directories. Tests construct this directly with temp dirs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    /// `~/.config/harness` on Linux.
    pub user_config_dir: PathBuf,
    /// `~/.local/share/harness` on Linux.
    pub user_data_dir: PathBuf,
}

impl Paths {
    /// Finds the platform directories for the current user.
    ///
    /// # Errors
    /// [`ConfigError::NoHome`] if the home directory cannot be determined.
    pub fn discover() -> Result<Self, ConfigError> {
        let base = directories::BaseDirs::new().ok_or(ConfigError::NoHome)?;
        Ok(Self {
            user_config_dir: base.config_dir().join("harness"),
            user_data_dir: base.data_dir().join("harness"),
        })
    }

    /// `~/.config/harness/config.toml`.
    #[must_use]
    pub fn user_config_file(&self) -> PathBuf {
        self.user_config_dir.join("config.toml")
    }

    /// `~/.local/share/harness/trust.toml`.
    #[must_use]
    pub fn trust_file(&self) -> PathBuf {
        self.user_data_dir.join("trust.toml")
    }

    /// `~/.local/share/harness/logs`.
    #[must_use]
    pub fn log_dir(&self) -> PathBuf {
        self.user_data_dir.join("logs")
    }

    /// `~/.config/harness/rules.toml`: the user's policy rules (Tech spec 3.6).
    #[must_use]
    pub fn user_rules_file(&self) -> PathBuf {
        self.user_config_dir.join("rules.toml")
    }

    /// `<project>/.harness/rules.toml`: the project's policy rules (used only
    /// when the project is trusted).
    #[must_use]
    pub fn project_rules_file(project: &Path) -> PathBuf {
        project.join(".harness").join("rules.toml")
    }

    /// `<project>/.harness/config.toml`.
    #[must_use]
    pub fn project_config_file(project: &Path) -> PathBuf {
        project.join(".harness").join("config.toml")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derived_paths_follow_the_storage_layout() {
        let p = Paths {
            user_config_dir: "/c/harness".into(),
            user_data_dir: "/d/harness".into(),
        };
        assert_eq!(
            p.user_config_file(),
            PathBuf::from("/c/harness/config.toml")
        );
        assert_eq!(p.trust_file(), PathBuf::from("/d/harness/trust.toml"));
        assert_eq!(p.log_dir(), PathBuf::from("/d/harness/logs"));
        assert_eq!(p.user_rules_file(), PathBuf::from("/c/harness/rules.toml"));
        assert_eq!(
            Paths::project_rules_file(Path::new("/p")),
            PathBuf::from("/p/.harness/rules.toml")
        );
        assert_eq!(
            Paths::project_config_file(Path::new("/p")),
            PathBuf::from("/p/.harness/config.toml")
        );
    }

    #[test]
    fn discover_ends_in_harness_directories() {
        if let Ok(p) = Paths::discover() {
            assert!(p.user_config_dir.ends_with("harness"));
            assert!(p.user_data_dir.ends_with("harness"));
        }
    }
}
