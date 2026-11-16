//! [`ConfigLoader`]: merges the configuration layers in precedence order (SDD 2.10).

use std::path::{Path, PathBuf};

use toml::Table;

use crate::{Config, ConfigError, Paths, TrustStore, deep_merge, env_layer};

/// A layer that contributed to (or was skipped from) the final configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Layer {
    /// Built-in defaults.
    Defaults,
    /// The user config file.
    User(PathBuf),
    /// The project config file (project is trusted).
    Project(PathBuf),
    /// The project config file was ignored because the project is not trusted.
    ProjectSkipped(PathBuf),
    /// The resolved persona's settings.
    Persona,
    /// `HARNESS_*` environment variables.
    Environment,
    /// Command-line flags and slash commands.
    Cli,
}

/// The result of loading configuration.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedConfig {
    /// The merged, validated configuration.
    pub config: Config,
    /// Layers in the order they were applied (skipped ones included, for `harness config show`).
    pub layers: Vec<Layer>,
    /// Whether the project directory is trusted.
    pub project_trusted: bool,
}

/// Builds the configuration from its layers, lowest precedence first:
/// defaults, user file, project file (trusted projects only), persona,
/// environment, CLI.
///
/// # Examples
/// ```
/// use harness_config::{ConfigLoader, Paths};
/// let dir = tempfile::tempdir().unwrap();
/// let paths = Paths { user_config_dir: dir.path().join("c"), user_data_dir: dir.path().join("d") };
/// let loaded = ConfigLoader::new(paths)
///     .env([("HARNESS_DEFAULT_PERSONA".to_string(), "coder".to_string())])
///     .load()
///     .unwrap();
/// assert_eq!(loaded.config.default_persona, "coder");
/// ```
#[derive(Debug, Clone)]
pub struct ConfigLoader {
    paths: Paths,
    project_dir: Option<PathBuf>,
    env: Vec<(String, String)>,
    persona: Option<Table>,
    cli: Option<Table>,
}

impl ConfigLoader {
    /// Starts a loader with only defaults and the user file.
    #[must_use]
    pub fn new(paths: Paths) -> Self {
        Self {
            paths,
            project_dir: None,
            env: Vec::new(),
            persona: None,
            cli: None,
        }
    }

    /// Sets the project directory (its `.harness/config.toml` loads only if trusted).
    #[must_use]
    pub fn project_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.project_dir = Some(dir.into());
        self
    }

    /// Sets the environment to read `HARNESS_*` variables from
    /// (pass `std::env::vars()` in production).
    #[must_use]
    pub fn env(mut self, vars: impl IntoIterator<Item = (String, String)>) -> Self {
        self.env = vars.into_iter().collect();
        self
    }

    /// Adds the resolved persona's settings as a layer.
    #[must_use]
    pub fn persona_layer(mut self, layer: Table) -> Self {
        self.persona = Some(layer);
        self
    }

    /// Adds command-line overrides as the highest-precedence layer.
    #[must_use]
    pub fn cli_overrides(mut self, layer: Table) -> Self {
        self.cli = Some(layer);
        self
    }

    /// Loads, merges and validates every layer.
    ///
    /// Each layer is checked against the schema on its own first, so an
    /// unknown key or bad value is reported with the file (or `environment`,
    /// `cli`) it came from.
    ///
    /// # Errors
    /// [`ConfigError::Io`] for unreadable files, [`ConfigError::Parse`] for bad
    /// TOML or unknown keys, [`ConfigError::Invalid`] for out-of-range values.
    pub fn load(&self) -> Result<LoadedConfig, ConfigError> {
        let mut layers = vec![Layer::Defaults];
        let mut merged = Table::try_from(Config::default())
            .map_err(|e| ConfigError::Invalid(format!("defaults do not serialise: {e}")))?;

        let user = self.paths.user_config_file();
        if let Some(table) = read_table(&user)? {
            check_layer(&table, &user.display().to_string())?;
            deep_merge(&mut merged, table);
            layers.push(Layer::User(user));
        }

        let mut project_trusted = false;
        if let Some(dir) = &self.project_dir {
            project_trusted = TrustStore::load(self.paths.trust_file())?.is_trusted(dir);
            let file = Paths::project_config_file(dir);
            if project_trusted {
                if let Some(table) = read_table(&file)? {
                    check_layer(&table, &file.display().to_string())?;
                    deep_merge(&mut merged, table);
                    layers.push(Layer::Project(file));
                }
            } else if file.is_file() {
                // Untrusted project config is never read, only reported.
                layers.push(Layer::ProjectSkipped(file));
            }
        }

        if let Some(table) = &self.persona {
            check_layer(table, "persona")?;
            deep_merge(&mut merged, table.clone());
            layers.push(Layer::Persona);
        }

        let env = env_layer(self.env.iter().cloned());
        if !env.is_empty() {
            check_layer(&env, "environment")?;
            deep_merge(&mut merged, env);
            layers.push(Layer::Environment);
        }

        if let Some(table) = &self.cli {
            check_layer(table, "cli")?;
            deep_merge(&mut merged, table.clone());
            layers.push(Layer::Cli);
        }

        let config: Config = merged.try_into().map_err(|e| ConfigError::Parse {
            source_name: "merged configuration".into(),
            message: e.to_string(),
        })?;
        config.validate()?;
        Ok(LoadedConfig {
            config,
            layers,
            project_trusted,
        })
    }
}

/// Reads a TOML file as a table; `Ok(None)` if the file does not exist.
fn read_table(path: &Path) -> Result<Option<Table>, ConfigError> {
    match std::fs::read_to_string(path) {
        Ok(text) => toml::from_str(&text)
            .map(Some)
            .map_err(|e| ConfigError::Parse {
                source_name: path.display().to_string(),
                message: e.to_string(),
            }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ConfigError::io(path, &e)),
    }
}

/// Checks that a single layer matches the schema on its own.
fn check_layer(layer: &Table, source_name: &str) -> Result<(), ConfigError> {
    layer
        .clone()
        .try_into::<Config>()
        .map(|_| ())
        .map_err(|e| ConfigError::Parse {
            source_name: source_name.to_owned(),
            message: e.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PolicyProfile;

    struct Env {
        _home: tempfile::TempDir,
        paths: Paths,
        project: tempfile::TempDir,
    }

    fn setup() -> Env {
        let home = tempfile::tempdir().unwrap();
        let paths = Paths {
            user_config_dir: home.path().join("config"),
            user_data_dir: home.path().join("data"),
        };
        Env {
            _home: home,
            paths,
            project: tempfile::tempdir().unwrap(),
        }
    }

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    fn trust(env: &Env) {
        TrustStore::load(env.paths.trust_file())
            .unwrap()
            .trust(env.project.path())
            .unwrap();
    }

    #[test]
    fn no_files_gives_defaults() {
        let env = setup();
        let loaded = ConfigLoader::new(env.paths.clone()).load().unwrap();
        assert_eq!(loaded.config, Config::default());
        assert_eq!(loaded.layers, vec![Layer::Defaults]);
    }

    #[test]
    fn user_file_overrides_defaults() {
        let env = setup();
        write(&env.paths.user_config_file(), "default_persona = \"coder\"");
        let loaded = ConfigLoader::new(env.paths.clone()).load().unwrap();
        assert_eq!(loaded.config.default_persona, "coder");
        assert_eq!(
            loaded.layers,
            vec![Layer::Defaults, Layer::User(env.paths.user_config_file())]
        );
    }

    #[test]
    fn untrusted_project_file_is_skipped_and_reported() {
        let env = setup();
        let file = Paths::project_config_file(env.project.path());
        write(&file, "[policy]\nprofile = \"full\"");
        let loaded = ConfigLoader::new(env.paths.clone())
            .project_dir(env.project.path())
            .load()
            .unwrap();
        assert_eq!(loaded.config.policy.profile, PolicyProfile::Auto);
        assert!(!loaded.project_trusted);
        assert!(loaded.layers.contains(&Layer::ProjectSkipped(file)));
    }

    #[test]
    fn trusted_project_file_is_applied() {
        let env = setup();
        trust(&env);
        let file = Paths::project_config_file(env.project.path());
        write(&file, "[policy]\nprofile = \"read-only\"");
        let loaded = ConfigLoader::new(env.paths.clone())
            .project_dir(env.project.path())
            .load()
            .unwrap();
        assert_eq!(loaded.config.policy.profile, PolicyProfile::ReadOnly);
        assert!(loaded.project_trusted);
        assert!(loaded.layers.contains(&Layer::Project(file)));
    }

    #[test]
    fn precedence_is_user_project_persona_env_cli() {
        let env = setup();
        trust(&env);
        write(
            &env.paths.user_config_file(),
            "default_persona = \"user\"\n[runtime]\nmax_parallel_tools = 1",
        );
        write(
            &Paths::project_config_file(env.project.path()),
            "default_persona = \"project\"\n[runtime]\nmax_parallel_agents = 2",
        );
        let persona: Table =
            toml::from_str("default_persona = \"persona\"\n[runtime]\nmax_agent_depth = 3")
                .unwrap();
        let cli: Table = toml::from_str("default_persona = \"cli\"").unwrap();
        let loaded = ConfigLoader::new(env.paths.clone())
            .project_dir(env.project.path())
            .persona_layer(persona)
            .env([
                ("HARNESS_DEFAULT_PERSONA".into(), "env".into()),
                ("HARNESS_RUNTIME__OP_CHANNEL_CAPACITY".into(), "9".into()),
            ])
            .cli_overrides(cli)
            .load()
            .unwrap();
        let c = &loaded.config;
        assert_eq!(c.default_persona, "cli");
        assert_eq!(
            (
                c.runtime.max_parallel_tools,
                c.runtime.max_parallel_agents,
                c.runtime.max_agent_depth,
                c.runtime.op_channel_capacity
            ),
            (1, 2, 3, 9)
        );
        assert_eq!(
            loaded
                .layers
                .iter()
                .map(std::mem::discriminant)
                .collect::<Vec<_>>(),
            [
                Layer::Defaults,
                Layer::User(PathBuf::new()),
                Layer::Project(PathBuf::new()),
                Layer::Persona,
                Layer::Environment,
                Layer::Cli
            ]
            .iter()
            .map(std::mem::discriminant)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn unknown_key_error_names_the_file() {
        let env = setup();
        write(&env.paths.user_config_file(), "[ui]\nthme = \"dark\"");
        let err = ConfigLoader::new(env.paths.clone()).load().unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("config.toml") && text.contains("thme"),
            "{text}"
        );
    }

    #[test]
    fn bad_env_value_error_names_the_environment() {
        let env = setup();
        let err = ConfigLoader::new(env.paths.clone())
            .env([("HARNESS_POLICY__PROFILE".into(), "yolo".into())])
            .load()
            .unwrap_err();
        assert!(err.to_string().starts_with("environment"), "{err}");
    }

    #[test]
    fn invalid_merged_value_is_rejected() {
        let env = setup();
        write(
            &env.paths.user_config_file(),
            "[context]\ncompact_at_ratio = 2.0",
        );
        assert!(matches!(
            ConfigLoader::new(env.paths.clone()).load(),
            Err(ConfigError::Invalid(_))
        ));
    }

    #[test]
    fn unreadable_user_config_is_an_io_error() {
        let env = setup();
        std::fs::create_dir_all(env.paths.user_config_file()).unwrap();
        assert!(matches!(
            ConfigLoader::new(env.paths.clone()).load(),
            Err(ConfigError::Io { .. })
        ));
    }

    #[test]
    fn syntax_error_is_a_parse_error_with_path() {
        let env = setup();
        write(&env.paths.user_config_file(), "this is not toml");
        match ConfigLoader::new(env.paths.clone()).load() {
            Err(ConfigError::Parse { source_name, .. }) => {
                assert!(source_name.ends_with("config.toml"))
            }
            other => panic!("expected parse error, got {other:?}"),
        }
    }
}
