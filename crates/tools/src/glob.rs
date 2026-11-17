//! The `glob` tool: find files by name pattern (Tech spec 3.5).

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use async_trait::async_trait;
use harness_ports::{RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::read_file::resolve;

/// Maximum paths returned by one call.
pub const MAX_GLOB_RESULTS: usize = 500;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    path: Option<String>,
}

/// Lists files matching a glob pattern, newest first.
///
/// Respects `.gitignore`, `.ignore` and hidden-file rules, like ripgrep. `*`
/// does not cross directories; use `**/` to search recursively.
#[derive(Debug, Clone, Copy, Default)]
pub struct Glob;

/// Paths under `root` matching `pattern`, newest first, plus the total count.
///
/// # Errors
/// [`ToolError::InvalidInput`] for a bad pattern.
pub(crate) fn find(
    root: &Path,
    pattern: &str,
    limit: usize,
) -> Result<(Vec<String>, usize), ToolError> {
    let matcher = globset::GlobBuilder::new(pattern)
        .literal_separator(true)
        .build()
        .map_err(|e| ToolError::InvalidInput(format!("bad glob pattern: {e}")))?
        .compile_matcher();
    let mut found: Vec<(SystemTime, String)> = ignore::WalkBuilder::new(root)
        .build()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_some_and(|t| t.is_file()))
        .filter_map(|entry| {
            let rel = entry.path().strip_prefix(root).ok()?.to_path_buf();
            if !matcher.is_match(&rel) {
                return None;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .unwrap_or(SystemTime::UNIX_EPOCH);
            Some((mtime, rel.to_string_lossy().into_owned()))
        })
        .collect();
    // Newest first; ties by path so the order is stable.
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let total = found.len();
    Ok((
        found.into_iter().take(limit).map(|(_, p)| p).collect(),
        total,
    ))
}

#[async_trait]
impl Tool for Glob {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("glob").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Find files by name pattern, newest first (max 500). Respects \
                .gitignore. `*` matches within one directory; use `**/` to search \
                recursively, for example `**/*.rs` or `src/**/test_*.py`."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Glob pattern relative to path."},
                    "path": {"type": "string", "description": "Directory to search (default: the working directory)."}
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, input: &Value, ctx: &RiskCtx) -> Risk {
        let root = input["path"]
            .as_str()
            .map_or_else(|| ctx.cwd.clone(), |p| resolve(&ctx.cwd, p));
        Risk::ReadOnly { paths: vec![root] }
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let root: PathBuf = input
            .path
            .as_deref()
            .map_or_else(|| ctx.cwd.clone(), |p| resolve(&ctx.cwd, p));
        if !root.is_dir() {
            return Err(ToolError::InvalidInput(format!(
                "{} is not a directory",
                input.path.as_deref().unwrap_or(".")
            )));
        }
        let pattern = input.pattern.clone();
        // Walking a large tree is blocking file-system work.
        let (paths, total) =
            tokio::task::spawn_blocking(move || find(&root, &pattern, MAX_GLOB_RESULTS))
                .await
                .map_err(|e| ToolError::Failed(format!("search stopped: {e}")))??;
        let mut text = if paths.is_empty() {
            format!("No files match {}.", input.pattern)
        } else {
            paths.join("\n")
        };
        if total > paths.len() {
            text.push_str(&format!(
                "\n[showing the {} newest of {total} matches; narrow the pattern to see the rest]",
                paths.len()
            ));
        }
        Ok(ToolOutput {
            content: vec![ContentBlock::text(text)],
            summary: format!("{total} files match {}", input.pattern),
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use std::time::Duration;

    fn touch(dir: &Path, rel: &str, age_s: u64) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, rel).unwrap();
        let t = SystemTime::now() - Duration::from_secs(age_s);
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(t)
            .unwrap();
    }

    async fn glob(dir: &Path, input: Value) -> Result<String, ToolError> {
        let (c, _, _) = ctx(dir);
        let out = Glob.run(input, c).await?;
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => Ok(text.clone()),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn matches_recursively_newest_first() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "old.rs", 300);
        touch(dir.path(), "src/new.rs", 10);
        touch(dir.path(), "src/mid.rs", 100);
        touch(dir.path(), "README.md", 1);
        let out = glob(dir.path(), json!({"pattern": "**/*.rs"}))
            .await
            .unwrap();
        assert_eq!(out, "src/new.rs\nsrc/mid.rs\nold.rs");
    }

    #[tokio::test]
    async fn star_does_not_cross_directories() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "top.rs", 1);
        touch(dir.path(), "src/deep.rs", 1);
        assert_eq!(
            glob(dir.path(), json!({"pattern": "*.rs"})).await.unwrap(),
            "top.rs"
        );
    }

    #[tokio::test]
    async fn respects_gitignore() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "target/\n").unwrap();
        touch(dir.path(), "target/debug/gen.rs", 1);
        touch(dir.path(), "src/lib.rs", 1);
        assert_eq!(
            glob(dir.path(), json!({"pattern": "**/*.rs"}))
                .await
                .unwrap(),
            "src/lib.rs"
        );
    }

    #[tokio::test]
    async fn searches_under_a_given_path() {
        let dir = tempfile::tempdir().unwrap();
        touch(dir.path(), "a/x.txt", 1);
        touch(dir.path(), "b/y.txt", 1);
        assert_eq!(
            glob(dir.path(), json!({"pattern": "*.txt", "path": "b"}))
                .await
                .unwrap(),
            "y.txt"
        );
    }

    #[tokio::test]
    async fn no_match_says_so() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            glob(dir.path(), json!({"pattern": "*.zig"})).await.unwrap(),
            "No files match *.zig."
        );
    }

    #[tokio::test]
    async fn results_are_limited_with_a_note() {
        let dir = tempfile::tempdir().unwrap();
        for i in 0..5 {
            touch(dir.path(), &format!("f{i}.txt"), i);
        }
        let (paths, total) = find(dir.path(), "*.txt", 2).unwrap();
        assert_eq!((paths, total), (vec!["f0.txt".into(), "f1.txt".into()], 5));
    }

    #[tokio::test]
    async fn bad_pattern_and_missing_dir_are_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            glob(dir.path(), json!({"pattern": "a[" })).await,
            Err(ToolError::InvalidInput(_))
        ));
        assert!(matches!(
            glob(dir.path(), json!({"pattern": "*", "path": "nope"})).await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[test]
    fn is_read_only_and_parallel_safe() {
        let ctx = RiskCtx {
            cwd: "/p".into(),
            writable_roots: vec![],
        };
        assert!(Glob.risk(&json!({"pattern": "*"}), &ctx).is_read_only());
        assert!(Glob.parallel_safe());
    }
}
