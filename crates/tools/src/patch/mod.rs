//! The `apply_patch` tool (Tech spec 3.5): multi-file edits in the Codex patch
//! format, applied all-or-nothing.
//!
//! - [`parse_patch`] turns text into a [`Patch`]
//! - [`plan`] computes every final file state in memory, or fails with nothing changed
//! - [`ApplyPatch`] checks the read ledger, plans, then commits: every new file
//!   is staged as a temporary file first, then renamed into place; if a step
//!   fails, the files already changed are restored

mod apply;
mod parse;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use harness_ports::{
    LedgerCheck, ReadLedger, RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec,
};
use harness_types::{ContentBlock, Risk, TaskId};
use serde::Deserialize;
use serde_json::{Value, json};

pub use apply::{FileChange, PatchFs, PlanError, normalize, plan};
pub use parse::{Hunk, HunkLine, ParseError, Patch, PatchOp, parse_patch};

use crate::fs_write::{record_written, tmp_name};
use crate::read_file::resolve;
use crate::support::stamp_of;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    patch: String,
}

/// Applies a patch in the Codex format to one or more files, atomically.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyPatch;

/// Reads files under the working directory for [`plan`], refusing any file the
/// model read earlier that has changed on disk since (FR-TOOL-03).
struct DiskFs<'a> {
    cwd: &'a Path,
    ledger: &'a dyn ReadLedger,
}

impl PatchFs for DiskFs<'_> {
    fn read(&self, path: &str) -> Result<Option<String>, String> {
        let full = self.cwd.join(path);
        let metadata = match std::fs::metadata(&full) {
            Ok(m) => m,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e.to_string()),
        };
        if metadata.is_dir() {
            return Err("it is a directory".into());
        }
        let bytes = std::fs::read(&full).map_err(|e| e.to_string())?;
        // Unlike edit_file, a patch may change a file that was never read: its
        // context lines prove what the model expects. A stale read is refused.
        if self.ledger.check(&full, &stamp_of(&bytes, &metadata)) == LedgerCheck::ChangedSinceRead {
            return Err("it changed on disk since you read it; read it again".into());
        }
        String::from_utf8(bytes)
            .map(Some)
            .map_err(|_| "it is not UTF-8 text".into())
    }
}

/// Writes the planned changes, restoring the originals if any step fails.
///
/// # Errors
/// A message naming the file and the I/O error.
fn commit(cwd: &Path, changes: &[FileChange], task: TaskId) -> Result<(), String> {
    // Stage every new file next to its target first: most failures (permissions,
    // disk full) happen here, before anything visible has changed.
    let mut staged: Vec<Option<PathBuf>> = Vec::with_capacity(changes.len());
    for change in changes {
        let Some(after) = &change.after else {
            staged.push(None);
            continue;
        };
        let target = cwd.join(&change.path);
        let result = (|| {
            let parent = target.parent().ok_or("no parent directory")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let name = target.file_name().ok_or("no file name")?;
            let tmp = parent.join(tmp_name(name, task));
            std::fs::write(&tmp, after).map_err(|e| e.to_string())?;
            if let Ok(m) = std::fs::metadata(&target) {
                std::fs::set_permissions(&tmp, m.permissions()).map_err(|e| e.to_string())?;
            }
            Ok::<_, String>(tmp)
        })();
        match result {
            Ok(tmp) => staged.push(Some(tmp)),
            Err(e) => {
                remove_staged(&staged);
                return Err(format!("cannot write {}: {e}", change.path));
            }
        }
    }
    for (i, change) in changes.iter().enumerate() {
        let target = cwd.join(&change.path);
        let step = match &staged[i] {
            Some(tmp) => std::fs::rename(tmp, &target),
            None => std::fs::remove_file(&target),
        };
        if let Err(e) = step {
            restore(cwd, &changes[..i]);
            remove_staged(&staged[i..]);
            return Err(format!("cannot update {}: {e}", change.path));
        }
    }
    Ok(())
}

fn remove_staged(staged: &[Option<PathBuf>]) {
    for tmp in staged.iter().flatten() {
        let _ = std::fs::remove_file(tmp);
    }
}

/// Puts back the original state of files already changed (best effort).
fn restore(cwd: &Path, done: &[FileChange]) {
    for change in done.iter().rev() {
        let target = cwd.join(&change.path);
        let _ = match &change.before {
            Some(before) => std::fs::write(&target, before),
            None => std::fs::remove_file(&target),
        };
    }
}

/// Added and removed line counts per final path, taken from the patch itself.
fn line_counts(patch: &Patch) -> HashMap<String, (usize, usize)> {
    let mut counts: HashMap<String, (usize, usize)> = HashMap::new();
    for op in &patch.ops {
        match op {
            PatchOp::Add { path, lines } => {
                counts.entry(normalize(path)).or_default().0 += lines.len();
            }
            PatchOp::Delete { .. } => {}
            PatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let key = normalize(move_to.as_deref().unwrap_or(path));
                let entry = counts.entry(key).or_default();
                for line in hunks.iter().flat_map(|h| &h.lines) {
                    match line {
                        HunkLine::Add(_) => entry.0 += 1,
                        HunkLine::Remove(_) => entry.1 += 1,
                        HunkLine::Context(_) => {}
                    }
                }
            }
        }
    }
    counts
}

/// The result text: one line per file, `A`dded, `M`odified or `D`eleted.
fn report(changes: &[FileChange], counts: &HashMap<String, (usize, usize)>) -> String {
    let mut out = format!(
        "Applied the patch to {} file{}:",
        changes.len(),
        if changes.len() == 1 { "" } else { "s" }
    );
    for c in changes {
        let (added, removed) = counts.get(&c.path).copied().unwrap_or_default();
        let line = match (&c.before, &c.after) {
            (None, Some(_)) => format!("A {} (+{added})", c.path),
            (Some(_), None) => format!("D {}", c.path),
            _ => format!("M {} (+{added} -{removed})", c.path),
        };
        out.push('\n');
        out.push_str(&line);
    }
    out
}

#[async_trait]
impl Tool for ApplyPatch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("apply_patch").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Edit, create, delete or rename files with one patch, applied \
                all-or-nothing. Format:\n\
                *** Begin Patch\n\
                *** Add File: path/new.rs\n\
                +line\n\
                *** Delete File: path/old.rs\n\
                *** Update File: path/lib.rs\n\
                *** Move to: path/renamed.rs   (optional)\n\
                @@ fn some_function()          (optional anchor: a line at or before the change)\n \
                context line\n\
                -removed line\n\
                +added line\n\
                *** End Patch\n\
                Paths are relative to the working directory. Give about 3 lines of context \
                around each change so every hunk matches exactly one place."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "patch": {"type": "string", "description": "The full patch text, from *** Begin Patch to *** End Patch."}
                },
                "required": ["patch"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: true,
        }
    }

    fn risk(&self, input: &Value, ctx: &RiskCtx) -> Risk {
        // An unparsable patch changes nothing; run() reports the parse error.
        let paths = input["patch"]
            .as_str()
            .and_then(|p| parse_patch(p).ok())
            .map(|p| {
                p.paths()
                    .into_iter()
                    .map(|path| resolve(&ctx.cwd, &normalize(path)))
                    .collect()
            })
            .unwrap_or_default();
        Risk::Write { paths }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let patch = parse_patch(&input.patch)
            .map_err(|e| ToolError::InvalidInput(format!("{e}. Nothing was changed.")))?;
        let cwd = ctx.cwd.clone();
        let ledger: Arc<dyn ReadLedger> = Arc::clone(&ctx.ledger);
        let task = ctx.task;
        let counts = line_counts(&patch);
        // Reading and writing several files is blocking work.
        let changes = tokio::task::spawn_blocking(move || {
            let fs = DiskFs {
                cwd: &cwd,
                ledger: ledger.as_ref(),
            };
            let changes = plan(&patch, &fs)
                .map_err(|e| ToolError::Failed(format!("{e}. Nothing was changed.")))?;
            commit(&cwd, &changes, task)
                .map_err(|e| ToolError::Failed(format!("{e}. Nothing was changed.")))?;
            Ok::<_, ToolError>(changes)
        })
        .await
        .map_err(|e| ToolError::Failed(format!("patch stopped: {e}")))??;

        let mut touched = Vec::with_capacity(changes.len());
        for change in &changes {
            let path = ctx.cwd.join(&change.path);
            if change.after.is_some() {
                record_written(ctx.ledger.as_ref(), &path).await;
            }
            touched.push(path);
        }
        Ok(ToolOutput {
            content: vec![ContentBlock::text(report(&changes, &counts))],
            summary: format!("patched {} files", changes.len()),
            touched,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadFile;
    use crate::test_support::ctx;

    fn patch(body: &str) -> Value {
        json!({ "patch": format!("*** Begin Patch\n{body}\n*** End Patch") })
    }

    fn text(out: &ToolOutput) -> &str {
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => text,
            other => panic!("unexpected {other:?}"),
        }
    }

    fn read(dir: &Path, p: &str) -> String {
        std::fs::read_to_string(dir.join(p)).unwrap()
    }

    #[tokio::test]
    async fn applies_add_update_delete_and_reports_each_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("lib.rs"), "a\nb\nc\n").unwrap();
        std::fs::write(dir.path().join("old.txt"), "bye\n").unwrap();
        let (c, _, _) = ctx(dir.path());
        let out = ApplyPatch
            .run(
                patch("*** Add File: src/new.rs\n+fn x() {}\n*** Update File: lib.rs\n@@\n a\n-b\n+B\n+B2\n*** Delete File: old.txt"),
                c,
            )
            .await
            .unwrap();
        assert_eq!(
            text(&out),
            "Applied the patch to 3 files:\nA src/new.rs (+1)\nM lib.rs (+2 -1)\nD old.txt"
        );
        assert_eq!(read(dir.path(), "src/new.rs"), "fn x() {}\n");
        assert_eq!(read(dir.path(), "lib.rs"), "a\nB\nB2\nc\n");
        assert!(!dir.path().join("old.txt").exists());
        assert_eq!(out.touched.len(), 3);
    }

    #[tokio::test]
    async fn a_failing_hunk_changes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n").unwrap();
        std::fs::write(dir.path().join("b.txt"), "2\n").unwrap();
        let (c, _, _) = ctx(dir.path());
        let err = ApplyPatch
            .run(
                patch(
                    "*** Update File: a.txt\n@@\n-1\n+one\n*** Update File: b.txt\n@@\n-nope\n+x",
                ),
                c,
            )
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("b.txt, hunk 1") && m.ends_with("Nothing was changed.")),
            "{err}"
        );
        assert_eq!(read(dir.path(), "a.txt"), "1\n");
        assert_eq!(read(dir.path(), "b.txt"), "2\n");
    }

    #[tokio::test]
    async fn parse_errors_are_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let err = ApplyPatch
            .run(json!({"patch": "not a patch"}), c)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::InvalidInput(m) if m.contains("patch line 1")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn a_file_changed_since_it_was_read_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\n").unwrap();
        let (c, _, _) = ctx(dir.path());
        ReadFile
            .run(json!({"path": "a.txt"}), c.clone())
            .await
            .unwrap();
        std::fs::write(dir.path().join("a.txt"), "1\nchanged\n").unwrap();
        let err = ApplyPatch
            .run(patch("*** Update File: a.txt\n@@\n-1\n+one"), c)
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("changed on disk")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn patched_files_can_be_edited_again_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        ApplyPatch
            .run(patch("*** Add File: a.txt\n+1"), c.clone())
            .await
            .unwrap();
        crate::EditFile
            .run(json!({"path": "a.txt", "old": "1", "new": "2"}), c)
            .await
            .unwrap();
        assert_eq!(read(dir.path(), "a.txt"), "2\n");
    }

    #[tokio::test]
    async fn rename_moves_the_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "x\n").unwrap();
        let (c, _, _) = ctx(dir.path());
        let out = ApplyPatch
            .run(
                patch("*** Update File: a.rs\n*** Move to: b/c.rs\n@@\n-x\n+y"),
                c,
            )
            .await
            .unwrap();
        assert!(!dir.path().join("a.rs").exists());
        assert_eq!(read(dir.path(), "b/c.rs"), "y\n");
        assert_eq!(
            text(&out),
            "Applied the patch to 2 files:\nD a.rs\nA b/c.rs (+1)"
        );
    }

    #[test]
    fn commit_restores_files_when_a_step_fails() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let changes = vec![
            FileChange {
                path: "a.txt".into(),
                before: Some("old".into()),
                after: Some("new".into()),
            },
            // Deleting a file that is not there fails the second step.
            FileChange {
                path: "missing.txt".into(),
                before: Some("x".into()),
                after: None,
            },
        ];
        let err = commit(dir.path(), &changes, TaskId::new()).unwrap_err();
        assert!(err.contains("missing.txt"), "{err}");
        assert_eq!(read(dir.path(), "a.txt"), "old");
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(leftovers, ["a.txt"]);
    }

    #[test]
    fn risk_lists_every_path_including_move_targets() {
        let ctx = RiskCtx {
            cwd: "/p".into(),
            writable_roots: vec![],
        };
        let risk = ApplyPatch.risk(
            &patch("*** Update File: ./a\n*** Move to: b\n@@\n-x\n+y\n*** Add File: c\n+1"),
            &ctx,
        );
        assert_eq!(
            risk,
            Risk::Write {
                paths: vec!["/p/a".into(), "/p/b".into(), "/p/c".into()]
            }
        );
        assert_eq!(
            ApplyPatch.risk(&json!({"patch": "junk"}), &ctx),
            Risk::Write { paths: vec![] }
        );
    }
}
