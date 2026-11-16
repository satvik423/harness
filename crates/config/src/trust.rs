//! [`TrustStore`]: the list of projects whose config, rules, hooks, personas
//! and skills may be loaded (FR-SAFE-05).

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::ConfigError;

/// On-disk format of the trust file.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustFile {
    #[serde(default)]
    version: u32,
    projects: BTreeSet<PathBuf>,
}

/// Projects the user has trusted with `harness trust`.
///
/// Paths are stored canonicalised, so `./app`, `../x/app` and symlinks to it
/// are the same project. Trust is exact: trusting a directory does not trust
/// its subdirectories or parents. Saving is atomic (write a temp file, then rename).
#[derive(Debug, Clone)]
pub struct TrustStore {
    file: PathBuf,
    projects: BTreeSet<PathBuf>,
}

impl TrustStore {
    /// Loads the trust file, or starts empty if it does not exist yet.
    ///
    /// # Errors
    /// [`ConfigError::Io`] if the file exists but cannot be read, or
    /// [`ConfigError::Parse`] if it is malformed.
    pub fn load(file: impl Into<PathBuf>) -> Result<Self, ConfigError> {
        let file = file.into();
        let projects = match std::fs::read_to_string(&file) {
            Ok(text) => {
                toml::from_str::<TrustFile>(&text)
                    .map_err(|e| ConfigError::Parse {
                        source_name: file.display().to_string(),
                        message: e.to_string(),
                    })?
                    .projects
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeSet::new(),
            Err(e) => return Err(ConfigError::io(&file, &e)),
        };
        Ok(Self { file, projects })
    }

    /// `true` if `dir` (after canonicalisation) is trusted. A directory that
    /// does not exist is never trusted.
    #[must_use]
    pub fn is_trusted(&self, dir: &Path) -> bool {
        dir.canonicalize().is_ok_and(|c| self.projects.contains(&c))
    }

    /// Trusts `dir` and saves. Returns `true` if it was not trusted before.
    ///
    /// # Errors
    /// [`ConfigError::Io`] if `dir` cannot be canonicalised or the file cannot be written.
    pub fn trust(&mut self, dir: &Path) -> Result<bool, ConfigError> {
        let canonical = dir.canonicalize().map_err(|e| ConfigError::io(dir, &e))?;
        let added = self.projects.insert(canonical);
        if added {
            self.save()?;
        }
        Ok(added)
    }

    /// Removes trust for `dir` and saves. Returns `true` if it was trusted.
    ///
    /// # Errors
    /// [`ConfigError::Io`] if the file cannot be written.
    pub fn untrust(&mut self, dir: &Path) -> Result<bool, ConfigError> {
        // A deleted project can't be canonicalised; fall back to the path as given.
        let key = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
        let removed = self.projects.remove(&key);
        if removed {
            self.save()?;
        }
        Ok(removed)
    }

    /// All trusted projects, sorted.
    pub fn projects(&self) -> impl Iterator<Item = &Path> {
        self.projects.iter().map(PathBuf::as_path)
    }

    fn save(&self) -> Result<(), ConfigError> {
        if let Some(parent) = self.file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| ConfigError::io(parent, &e))?;
        }
        let body = TrustFile {
            version: 1,
            projects: self.projects.clone(),
        };
        let text = toml::to_string(&body).map_err(|e| ConfigError::Invalid(e.to_string()))?;
        // Write-then-rename so a crash never leaves a half-written trust file.
        let tmp = self.file.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(|e| ConfigError::io(&tmp, &e))?;
        std::fs::rename(&tmp, &self.file).map_err(|e| ConfigError::io(&self.file, &e))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_means_nothing_is_trusted() {
        let home = tempfile::tempdir().unwrap();
        let store = TrustStore::load(home.path().join("trust.toml")).unwrap();
        assert!(!store.is_trusted(home.path()));
        assert_eq!(store.projects().count(), 0);
    }

    #[test]
    fn trusting_persists_across_reloads() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let file = home.path().join("data/trust.toml");
        let mut store = TrustStore::load(&file).unwrap();
        assert!(store.trust(project.path()).unwrap());
        assert!(
            !store.trust(project.path()).unwrap(),
            "second trust is a no-op"
        );
        let reloaded = TrustStore::load(&file).unwrap();
        assert!(reloaded.is_trusted(project.path()));
    }

    #[test]
    fn trust_is_matched_after_canonicalisation() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("sub")).unwrap();
        let mut store = TrustStore::load(home.path().join("t.toml")).unwrap();
        store.trust(project.path()).unwrap();
        assert!(store.is_trusted(&project.path().join("sub/..")));
    }

    #[test]
    fn trust_does_not_extend_to_subdirectories() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        std::fs::create_dir(project.path().join("child")).unwrap();
        let mut store = TrustStore::load(home.path().join("t.toml")).unwrap();
        store.trust(project.path()).unwrap();
        assert!(!store.is_trusted(&project.path().join("child")));
    }

    #[test]
    fn untrust_removes_and_persists() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let file = home.path().join("t.toml");
        let mut store = TrustStore::load(&file).unwrap();
        store.trust(project.path()).unwrap();
        assert!(store.untrust(project.path()).unwrap());
        assert!(!store.untrust(project.path()).unwrap());
        assert!(!TrustStore::load(&file).unwrap().is_trusted(project.path()));
    }

    #[test]
    fn save_leaves_no_temporary_files() {
        let home = tempfile::tempdir().unwrap();
        let project = tempfile::tempdir().unwrap();
        let mut store = TrustStore::load(home.path().join("t.toml")).unwrap();
        store.trust(project.path()).unwrap();
        let names: Vec<_> = std::fs::read_dir(home.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, vec![std::ffi::OsString::from("t.toml")]);
    }

    #[test]
    fn unreadable_file_is_an_io_error_naming_the_path() {
        let home = tempfile::tempdir().unwrap();
        // A directory where the file should be cannot be read as text.
        let err = TrustStore::load(home.path()).unwrap_err();
        assert!(
            matches!(&err, ConfigError::Io { path, .. } if path == home.path()),
            "{err:?}"
        );
    }

    #[test]
    fn trusting_a_missing_directory_is_an_io_error() {
        let home = tempfile::tempdir().unwrap();
        let mut store = TrustStore::load(home.path().join("t.toml")).unwrap();
        assert!(matches!(
            store.trust(&home.path().join("nope")),
            Err(ConfigError::Io { .. })
        ));
    }

    #[test]
    fn malformed_file_is_a_parse_error() {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("t.toml");
        std::fs::write(&file, "projects = 3").unwrap();
        assert!(matches!(
            TrustStore::load(&file),
            Err(ConfigError::Parse { .. })
        ));
    }
}
