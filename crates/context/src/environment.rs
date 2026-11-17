//! The environment block written into each segment opener.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::civil_date;

/// Status lines shown before summarising the rest.
const MAX_STATUS_LINES: usize = 20;

/// Git state at segment start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitInfo {
    /// Current branch (or `HEAD` when detached).
    pub branch: String,
    /// `git status --porcelain` lines, at most 20.
    pub status: Vec<String>,
    /// How many further status lines were omitted.
    pub more: usize,
}

/// Facts about the working environment, frozen when a segment opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    /// Working directory.
    pub cwd: PathBuf,
    /// Operating system, for example `linux`.
    pub os: String,
    /// UTC date `YYYY-MM-DD`.
    pub date: String,
    /// Git state, if `cwd` is in a repository.
    pub git: Option<GitInfo>,
    /// Extra lines, for example how approvals work in this session.
    pub notes: Vec<String>,
}

impl Environment {
    /// Collects the environment for `cwd`. Runs `git`; call it off the async
    /// runtime's worker threads (for example via `spawn_blocking`).
    #[must_use]
    pub fn gather(cwd: &Path, now_ms: i64) -> Self {
        Self {
            cwd: cwd.to_path_buf(),
            os: std::env::consts::OS.to_owned(),
            date: civil_date(now_ms),
            git: git_info(cwd),
            notes: Vec::new(),
        }
    }

    /// Renders the block as it appears in the segment opener.
    #[must_use]
    pub fn render(&self) -> String {
        let mut lines = vec![
            "<environment>".to_owned(),
            format!("Working directory: {}", self.cwd.display()),
            format!("Platform: {}", self.os),
            format!("Date: {}", self.date),
        ];
        match &self.git {
            None => lines.push("Not a git repository.".to_owned()),
            Some(git) => {
                lines.push(format!("Git branch: {}", git.branch));
                if git.status.is_empty() {
                    lines.push("Git status at session start: clean".to_owned());
                } else {
                    lines.push("Git status at session start:".to_owned());
                    lines.extend(git.status.iter().cloned());
                    if git.more > 0 {
                        lines.push(format!("… and {} more", git.more));
                    }
                }
            }
        }
        lines.extend(self.notes.iter().cloned());
        lines.push("</environment>".to_owned());
        lines.join("\n")
    }
}

/// Runs `git` in `cwd`; `None` if it is missing or fails.
fn run_git(cwd: &Path, args: &[&str]) -> Option<String> {
    let out = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
}

fn git_info(cwd: &Path) -> Option<GitInfo> {
    if run_git(cwd, &["rev-parse", "--is-inside-work-tree"])?.trim() != "true" {
        return None;
    }
    // `symbolic-ref` also works on an unborn branch (a repo with no commits yet).
    let branch = run_git(cwd, &["symbolic-ref", "--short", "-q", "HEAD"])
        .or_else(|| run_git(cwd, &["rev-parse", "--short", "HEAD"]))
        .map_or_else(|| "HEAD".to_owned(), |b| b.trim().to_owned());
    let status_text = run_git(cwd, &["status", "--porcelain"]).unwrap_or_default();
    let all: Vec<String> = status_text.lines().map(str::to_owned).collect();
    let more = all.len().saturating_sub(MAX_STATUS_LINES);
    let status = all.into_iter().take(MAX_STATUS_LINES).collect();
    Some(GitInfo {
        branch,
        status,
        more,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_includes_all_fields() {
        let env = Environment {
            cwd: "/proj".into(),
            os: "linux".into(),
            date: "2026-11-17".into(),
            git: Some(GitInfo {
                branch: "main".into(),
                status: vec![" M src/a.rs".into()],
                more: 3,
            }),
            notes: vec!["The user approves every command.".into()],
        };
        let text = env.render();
        for needle in [
            "<environment>",
            "Working directory: /proj",
            "Platform: linux",
            "Date: 2026-11-17",
            "Git branch: main",
            " M src/a.rs",
            "3 more",
            "The user approves every command.",
            "</environment>",
        ] {
            assert!(text.contains(needle), "missing {needle:?} in\n{text}");
        }
    }

    #[test]
    fn render_without_git_says_so() {
        let env = Environment {
            cwd: "/x".into(),
            os: "linux".into(),
            date: "d".into(),
            git: None,
            notes: vec![],
        };
        assert!(env.render().contains("Not a git repository"));
    }

    #[test]
    fn gather_outside_a_repo_has_no_git_info() {
        let dir = tempfile::tempdir().unwrap();
        let env = Environment::gather(dir.path(), 0);
        assert_eq!(env.git, None);
        assert_eq!(env.date, "1970-01-01");
        assert_eq!(env.os, std::env::consts::OS);
    }

    #[test]
    fn gather_inside_a_repo_reads_branch_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let ok = Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .is_ok_and(|s| s.success());
        if !ok {
            return; // git not installed: nothing to check
        }
        std::fs::write(dir.path().join("new.txt"), "x").unwrap();
        let git = Environment::gather(dir.path(), 0).git.expect("git info");
        assert!(!git.branch.is_empty());
        assert_eq!(git.status, vec!["?? new.txt"]);
    }
}
