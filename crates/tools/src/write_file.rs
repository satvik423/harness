//! The `write_file` tool: create a file or replace its contents (Tech spec 3.5).

use async_trait::async_trait;
use harness_ports::{RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::fs_write::{Existing, check_fresh, line_count, record_written, snapshot, write_atomic};
use crate::read_file::resolve;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    content: String,
}

/// Creates a file, or replaces the whole contents of one.
///
/// Replacing an existing file requires that it was read in this session and is
/// unchanged since (FR-TOOL-03). The write is atomic.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("write_file").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Create a file, or replace the entire contents of an existing file. \
                An existing file must be read with read_file first. Prefer edit_file or \
                apply_patch for changes to part of a file. Parent directories are created."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the working directory or absolute."},
                    "content": {"type": "string", "description": "The complete new contents."}
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: true,
        }
    }

    fn risk(&self, input: &Value, ctx: &RiskCtx) -> Risk {
        let paths = input["path"]
            .as_str()
            .map(|p| vec![resolve(&ctx.cwd, p)])
            .unwrap_or_default();
        Risk::Write { paths }
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let path = resolve(&ctx.cwd, &input.path);
        let existing = check_fresh(ctx.ledger.as_ref(), &path, &input.path).await?;
        if existing == Existing::File(input.content.clone().into_bytes()) {
            return Ok(ToolOutput {
                content: vec![ContentBlock::text(format!(
                    "{} already has exactly this content; nothing was written.",
                    input.path
                ))],
                summary: format!("unchanged {}", input.path),
                touched: Vec::new(),
            });
        }
        let before = match &existing {
            Existing::File(bytes) => Some(bytes.as_slice()),
            Existing::Missing => None,
        };
        snapshot(&ctx, &path, before, &input.path).await?;
        write_atomic(&path, input.content.as_bytes(), &input.path, ctx.task).await?;
        record_written(ctx.ledger.as_ref(), &path).await;
        let verb = match existing {
            Existing::Missing => "Created",
            Existing::File(_) => "Replaced",
        };
        let lines = line_count(&input.content);
        Ok(ToolOutput {
            content: vec![ContentBlock::text(format!(
                "{verb} {} ({lines} lines).",
                input.path
            ))],
            summary: format!("{} {}", verb.to_lowercase(), input.path),
            touched: vec![path],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadFile;
    use crate::test_support::ctx;

    async fn write(
        dir: &std::path::Path,
        ctx: ToolCtx,
        path: &str,
        content: &str,
    ) -> Result<ToolOutput, ToolError> {
        let _ = dir;
        WriteFile
            .run(json!({"path": path, "content": content}), ctx)
            .await
    }

    fn text(out: &ToolOutput) -> &str {
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => text,
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn creates_a_new_file_without_a_prior_read() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        let out = write(dir.path(), c, "src/new.rs", "fn main() {}\n")
            .await
            .unwrap();
        assert_eq!(text(&out), "Created src/new.rs (1 lines).");
        assert_eq!(out.touched, [dir.path().join("src/new.rs")]);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("src/new.rs")).unwrap(),
            "fn main() {}\n"
        );
    }

    #[tokio::test]
    async fn refuses_to_replace_a_file_that_was_not_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "keep me").unwrap();
        let (c, _, _) = ctx(dir.path());
        let err = write(dir.path(), c, "a.txt", "gone").await.unwrap_err();
        assert!(matches!(err, ToolError::Refused(_)), "{err}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "keep me"
        );
    }

    #[tokio::test]
    async fn replaces_a_file_after_reading_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "old").unwrap();
        let (c, _, _) = ctx(dir.path());
        ReadFile
            .run(json!({"path": "a.txt"}), c.clone())
            .await
            .unwrap();
        let out = write(dir.path(), c, "a.txt", "new\ntext\n").await.unwrap();
        assert_eq!(text(&out), "Replaced a.txt (2 lines).");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "new\ntext\n"
        );
    }

    #[tokio::test]
    async fn a_file_it_wrote_can_be_written_again_without_reading() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        write(dir.path(), c.clone(), "a.txt", "one").await.unwrap();
        write(dir.path(), c, "a.txt", "two").await.unwrap();
        assert_eq!(
            std::fs::read_to_string(dir.path().join("a.txt")).unwrap(),
            "two"
        );
    }

    #[tokio::test]
    async fn identical_content_is_not_rewritten() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _, _) = ctx(dir.path());
        write(dir.path(), c.clone(), "a.txt", "same").await.unwrap();
        let out = write(dir.path(), c, "a.txt", "same").await.unwrap();
        assert!(text(&out).contains("nothing was written"));
        assert!(out.touched.is_empty());
    }

    #[test]
    fn risk_is_a_write_of_the_resolved_path() {
        let ctx = RiskCtx {
            cwd: "/proj".into(),
            writable_roots: vec!["/proj".into()],
        };
        assert_eq!(
            WriteFile.risk(&json!({"path": "a/b.rs", "content": ""}), &ctx),
            Risk::Write {
                paths: vec!["/proj/a/b.rs".into()]
            }
        );
    }

    #[test]
    fn spec_streams_input_eagerly() {
        assert!(WriteFile.spec().eager_input_streaming);
    }
}
