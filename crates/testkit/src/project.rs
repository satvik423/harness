//! [`TempProject`]: a throwaway project directory for tests.

use std::path::{Component, Path, PathBuf};

use thiserror::Error;

/// Errors from building a temp project.
#[derive(Debug, Error)]
pub enum TempProjectError {
    /// A file or directory could not be created.
    #[error("cannot create {path}: {source}")]
    Io {
        /// The path.
        path: PathBuf,
        /// The OS error.
        source: std::io::Error,
    },
    /// A relative path tried to escape the project (absolute or containing `..`).
    #[error("path {0} must be relative and stay inside the project")]
    Escapes(PathBuf),
}

/// A temporary project directory that is deleted when dropped.
///
/// # Examples
/// ```
/// use harness_testkit::TempProject;
/// let p = TempProject::new().unwrap().with_file("src/main.rs", "fn main() {}").unwrap();
/// assert!(p.path().join("src/main.rs").is_file());
/// ```
#[derive(Debug)]
pub struct TempProject {
    dir: tempfile::TempDir,
}

impl TempProject {
    /// Creates an empty project directory.
    ///
    /// # Errors
    /// [`TempProjectError::Io`] if the temp directory cannot be created.
    pub fn new() -> Result<Self, TempProjectError> {
        tempfile::tempdir()
            .map(|dir| Self { dir })
            .map_err(|source| TempProjectError::Io {
                path: std::env::temp_dir(),
                source,
            })
    }

    /// Writes a file (creating parent directories) and returns the project.
    ///
    /// # Errors
    /// [`TempProjectError::Escapes`] for absolute paths or `..`, or
    /// [`TempProjectError::Io`] if writing fails.
    pub fn with_file(
        self,
        relative: impl AsRef<Path>,
        contents: &str,
    ) -> Result<Self, TempProjectError> {
        let relative = relative.as_ref();
        let escapes = relative.is_absolute()
            || relative
                .components()
                .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir));
        if escapes {
            return Err(TempProjectError::Escapes(relative.to_path_buf()));
        }
        let path = self.dir.path().join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|source| TempProjectError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        std::fs::write(&path, contents).map_err(|source| TempProjectError::Io { path, source })?;
        Ok(self)
    }

    /// The project root.
    #[must_use]
    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    /// `<root>/.harness`, created if missing.
    ///
    /// # Errors
    /// [`TempProjectError::Io`] if the directory cannot be created.
    pub fn harness_dir(&self) -> Result<PathBuf, TempProjectError> {
        let dir = self.dir.path().join(".harness");
        std::fs::create_dir_all(&dir).map_err(|source| TempProjectError::Io {
            path: dir.clone(),
            source,
        })?;
        Ok(dir)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_nested_files() {
        let p = TempProject::new()
            .unwrap()
            .with_file("a/b/c.txt", "hi")
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(p.path().join("a/b/c.txt")).unwrap(),
            "hi"
        );
    }

    #[test]
    fn rejects_paths_that_escape_the_project() {
        for bad in ["../x", "/etc/passwd", "a/../../x"] {
            let err = TempProject::new().unwrap().with_file(bad, "").unwrap_err();
            assert!(matches!(err, TempProjectError::Escapes(_)), "{bad}");
        }
    }

    #[test]
    fn harness_dir_is_created_inside_the_project() {
        let p = TempProject::new().unwrap();
        let dir = p.harness_dir().unwrap();
        assert!(dir.is_dir());
        assert_eq!(dir, p.path().join(".harness"));
    }

    #[test]
    fn errors_name_the_offending_path() {
        let p = TempProject::new().unwrap().with_file("file", "x").unwrap();
        // "file" is a regular file, so it cannot be a parent directory.
        let err = p.with_file("file/child", "y").unwrap_err();
        assert!(
            matches!(&err, TempProjectError::Io { path, .. } if path.ends_with("file")),
            "{err:?}"
        );
        assert!(err.to_string().contains("file"));
    }

    #[test]
    fn directory_is_removed_on_drop() {
        let p = TempProject::new().unwrap();
        let root = p.path().to_path_buf();
        drop(p);
        assert!(!root.exists());
    }
}
