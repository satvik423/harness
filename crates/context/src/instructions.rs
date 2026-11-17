//! Project instructions: the `AGENTS.md` chain from the git root down to the
//! working directory (system slot 5).

use std::path::{Path, PathBuf};

use crate::ContextError;

/// Maximum bytes of instructions included; later files are cut at this limit.
pub const MAX_INSTRUCTION_BYTES: usize = 32 * 1024;

/// One loaded instruction file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstructionFile {
    /// Where it was read from.
    pub path: PathBuf,
    /// Its text (possibly cut at [`MAX_INSTRUCTION_BYTES`]).
    pub text: String,
}

/// All instruction files for a working directory, root first.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ProjectInstructions {
    /// Files from the git root (or the working directory) down, in order.
    pub files: Vec<InstructionFile>,
    /// `true` if the size limit cut the text.
    pub truncated: bool,
}

/// The nearest ancestor of `start` (inclusive) containing `.git` (a directory or a worktree file).
#[must_use]
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .find(|dir| dir.join(".git").exists())
        .map(Path::to_path_buf)
}

/// Loads every `AGENTS.md` from the git root down to `cwd` (just `cwd` outside a repository).
///
/// # Errors
/// [`ContextError::Io`] if a file exists but cannot be read.
pub fn load_project_instructions(cwd: &Path) -> Result<ProjectInstructions, ContextError> {
    let root = find_git_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let mut dirs: Vec<&Path> = cwd
        .ancestors()
        .take_while(|dir| dir.starts_with(&root))
        .collect();
    dirs.reverse();

    let mut found = ProjectInstructions::default();
    let mut used = 0usize;
    for dir in dirs {
        let path = dir.join("AGENTS.md");
        let text = match std::fs::read_to_string(&path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                return Err(ContextError::Io {
                    path,
                    message: e.to_string(),
                });
            }
        };
        if used + text.len() > MAX_INSTRUCTION_BYTES {
            let mut cut = MAX_INSTRUCTION_BYTES - used;
            while !text.is_char_boundary(cut) {
                cut -= 1;
            }
            found.files.push(InstructionFile {
                path,
                text: text[..cut].to_owned(),
            });
            found.truncated = true;
            break;
        }
        used += text.len();
        found.files.push(InstructionFile { path, text });
    }
    Ok(found)
}

impl ProjectInstructions {
    /// Renders the files for the system prompt, or `None` when there are none.
    #[must_use]
    pub fn render(&self) -> Option<String> {
        if self.files.is_empty() {
            return None;
        }
        let mut out = String::from("# Project instructions (AGENTS.md)\n");
        for file in &self.files {
            out.push_str(&format!(
                "\n## {}\n\n{}\n",
                file.path.display(),
                file.text.trim_end()
            ));
        }
        if self.truncated {
            out.push_str(&format!(
                "\n[instructions truncated at {} KiB]\n",
                MAX_INSTRUCTION_BYTES / 1024
            ));
        }
        Some(out.trim_end().to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(path: &Path, text: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn finds_the_git_root_from_a_subdirectory() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        std::fs::create_dir_all(repo.path().join("a/b")).unwrap();
        assert_eq!(
            find_git_root(&repo.path().join("a/b")),
            Some(repo.path().to_path_buf())
        );
    }

    #[test]
    fn worktree_git_file_counts_as_a_root() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::write(repo.path().join(".git"), "gitdir: /elsewhere").unwrap();
        assert_eq!(find_git_root(repo.path()), Some(repo.path().to_path_buf()));
    }

    #[test]
    fn loads_the_chain_root_first() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        write(&repo.path().join("AGENTS.md"), "root rules");
        write(&repo.path().join("pkg/AGENTS.md"), "pkg rules");
        write(&repo.path().join("pkg/web/AGENTS.md"), "web rules");
        let found = load_project_instructions(&repo.path().join("pkg/web")).unwrap();
        let texts: Vec<&str> = found.files.iter().map(|f| f.text.as_str()).collect();
        assert_eq!(texts, ["root rules", "pkg rules", "web rules"]);
        assert!(!found.truncated);
    }

    #[test]
    fn sibling_directories_are_not_included() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        write(&repo.path().join("other/AGENTS.md"), "not mine");
        std::fs::create_dir_all(repo.path().join("pkg")).unwrap();
        assert!(
            load_project_instructions(&repo.path().join("pkg"))
                .unwrap()
                .files
                .is_empty()
        );
    }

    #[test]
    fn outside_a_repo_only_cwd_is_read() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("AGENTS.md"), "here");
        let found = load_project_instructions(dir.path()).unwrap();
        assert_eq!(found.files.len(), 1);
    }

    #[test]
    fn text_beyond_the_limit_is_cut() {
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir(repo.path().join(".git")).unwrap();
        write(
            &repo.path().join("AGENTS.md"),
            &"a".repeat(MAX_INSTRUCTION_BYTES - 10),
        );
        write(&repo.path().join("sub/AGENTS.md"), &"b".repeat(100));
        let found = load_project_instructions(&repo.path().join("sub")).unwrap();
        let total: usize = found.files.iter().map(|f| f.text.len()).sum();
        assert_eq!(total, MAX_INSTRUCTION_BYTES);
        assert!(found.truncated);
    }

    #[test]
    fn render_lists_each_file_with_its_path() {
        let found = ProjectInstructions {
            files: vec![InstructionFile {
                path: "/r/AGENTS.md".into(),
                text: "use pnpm".into(),
            }],
            truncated: true,
        };
        let text = found.render().unwrap();
        assert!(
            text.contains("/r/AGENTS.md") && text.contains("use pnpm"),
            "{text}"
        );
        assert!(text.contains("truncated"), "{text}");
        assert_eq!(ProjectInstructions::default().render(), None);
    }
}
