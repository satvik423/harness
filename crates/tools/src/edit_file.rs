//! The `edit_file` tool: replace an exact piece of text in a file (Tech spec 3.5).

use async_trait::async_trait;
use harness_ports::{RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::fs_write::{Existing, check_fresh, record_written, write_atomic};
use crate::read_file::resolve;

/// Lines of context shown around the first change in the result.
const SNIPPET_CONTEXT: usize = 3;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    old: String,
    new: String,
    #[serde(default)]
    replace_all: bool,
}

/// Replaces `old` with `new` in a file that was read in this session.
///
/// `old` must match exactly once unless `replace_all` is set. The file must be
/// unchanged since it was read (FR-TOOL-03). The write is atomic.
#[derive(Debug, Clone, Copy, Default)]
pub struct EditFile;

#[async_trait]
impl Tool for EditFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("edit_file").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Replace an exact piece of text in a file. Read the file with read_file \
                first. `old` must match the file exactly (including indentation) and only once; \
                include enough surrounding lines to make it unique, or set replace_all to change \
                every occurrence. Do not include the line-number prefixes from read_file."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, relative to the working directory or absolute."},
                    "old": {"type": "string", "description": "Exact text to replace."},
                    "new": {"type": "string", "description": "Replacement text."},
                    "replace_all": {"type": "boolean", "description": "Replace every occurrence (default false)."}
                },
                "required": ["path", "old", "new"],
                "additionalProperties": false
            }),
            strict: false,
            eager_input_streaming: false,
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
        if input.old.is_empty() {
            return Err(ToolError::InvalidInput(
                "old is empty; use write_file to create a file or replace all of it".into(),
            ));
        }
        if input.old == input.new {
            return Err(ToolError::InvalidInput(
                "old and new are identical; nothing would change".into(),
            ));
        }
        let path = resolve(&ctx.cwd, &input.path);
        let bytes = match check_fresh(ctx.ledger.as_ref(), &path, &input.path).await? {
            Existing::File(bytes) => bytes,
            Existing::Missing => {
                return Err(ToolError::InvalidInput(format!(
                    "{} does not exist; use write_file to create it",
                    input.path
                )));
            }
        };
        let text = String::from_utf8(bytes)
            .map_err(|_| ToolError::Refused(format!("{} is not UTF-8 text", input.path)))?;
        let count = text.matches(&input.old).count();
        match count {
            0 => {
                return Err(ToolError::Failed(format!(
                    "old text was not found in {}. Check whitespace and indentation, and copy the \
                     text from a fresh read_file (without line numbers).",
                    input.path
                )));
            }
            n if n > 1 && !input.replace_all => {
                return Err(ToolError::Failed(format!(
                    "old text matches {n} places in {}. Include more surrounding lines so it \
                     matches once, or set replace_all to change all {n}.",
                    input.path
                )));
            }
            _ => {}
        }
        let first = text.find(&input.old).unwrap_or(0);
        let updated = if input.replace_all {
            text.replace(&input.old, &input.new)
        } else {
            text.replacen(&input.old, &input.new, 1)
        };
        write_atomic(&path, updated.as_bytes(), &input.path, ctx.task).await?;
        record_written(ctx.ledger.as_ref(), &path).await;
        let replaced = if input.replace_all { count } else { 1 };
        let snippet = snippet(&updated, first, input.new.lines().count().max(1));
        Ok(ToolOutput {
            content: vec![ContentBlock::text(format!(
                "Edited {}: replaced {replaced} occurrence{}. The changed region now reads:\n{snippet}",
                input.path,
                if replaced == 1 { "" } else { "s" },
            ))],
            summary: format!("edited {}", input.path),
            touched: vec![path],
        })
    }
}

/// Numbered lines around byte offset `at`, covering `span` changed lines.
fn snippet(text: &str, at: usize, span: usize) -> String {
    let line = text[..at.min(text.len())].matches('\n').count();
    let start = line.saturating_sub(SNIPPET_CONTEXT);
    text.lines()
        .enumerate()
        .skip(start)
        .take(span + 2 * SNIPPET_CONTEXT)
        .map(|(i, l)| format!("{:>6}\t{l}", i + 1))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ReadFile;
    use crate::test_support::ctx;

    fn setup(content: &str) -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.rs"), content).unwrap();
        let (c, _, _) = ctx(dir.path());
        (dir, c)
    }

    async fn read(c: &ToolCtx) {
        ReadFile
            .run(json!({"path": "f.rs"}), c.clone())
            .await
            .unwrap();
    }

    async fn edit(c: &ToolCtx, input: Value) -> Result<ToolOutput, ToolError> {
        EditFile.run(input, c.clone()).await
    }

    fn file(dir: &tempfile::TempDir) -> String {
        std::fs::read_to_string(dir.path().join("f.rs")).unwrap()
    }

    #[tokio::test]
    async fn replaces_a_unique_match_and_shows_the_region() {
        let (dir, c) = setup("fn a() {\n    1\n}\n");
        read(&c).await;
        let out = edit(&c, json!({"path": "f.rs", "old": "    1", "new": "    2"}))
            .await
            .unwrap();
        assert_eq!(file(&dir), "fn a() {\n    2\n}\n");
        let ContentBlock::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(
            text.starts_with("Edited f.rs: replaced 1 occurrence."),
            "{text}"
        );
        assert!(text.contains("     2\t    2"), "{text}");
        assert_eq!(out.touched, [dir.path().join("f.rs")]);
    }

    #[tokio::test]
    async fn refuses_when_the_file_was_not_read() {
        let (dir, c) = setup("a");
        let err = edit(&c, json!({"path": "f.rs", "old": "a", "new": "b"}))
            .await
            .unwrap_err();
        assert!(matches!(err, ToolError::Refused(_)), "{err}");
        assert_eq!(file(&dir), "a");
    }

    #[tokio::test]
    async fn refuses_when_the_file_changed_since_read() {
        let (dir, c) = setup("a");
        read(&c).await;
        std::fs::write(dir.path().join("f.rs"), "a changed").unwrap();
        let err = edit(&c, json!({"path": "f.rs", "old": "a", "new": "b"}))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Refused(m) if m.contains("changed on disk")),
            "{err}"
        );
        assert_eq!(file(&dir), "a changed");
    }

    #[tokio::test]
    async fn ambiguous_match_fails_with_the_count() {
        let (dir, c) = setup("x\nx\nx\n");
        read(&c).await;
        let err = edit(&c, json!({"path": "f.rs", "old": "x", "new": "y"}))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("matches 3 places")),
            "{err}"
        );
        assert_eq!(file(&dir), "x\nx\nx\n");
    }

    #[tokio::test]
    async fn replace_all_changes_every_match() {
        let (dir, c) = setup("x\nx\nx\n");
        read(&c).await;
        let out = edit(
            &c,
            json!({"path": "f.rs", "old": "x", "new": "y", "replace_all": true}),
        )
        .await
        .unwrap();
        assert_eq!(file(&dir), "y\ny\ny\n");
        let ContentBlock::Text { text } = &out.content[0] else {
            panic!()
        };
        assert!(text.contains("replaced 3 occurrences"), "{text}");
    }

    #[tokio::test]
    async fn missing_text_fails_with_a_hint() {
        let (_dir, c) = setup("abc");
        read(&c).await;
        let err = edit(&c, json!({"path": "f.rs", "old": "zzz", "new": "y"}))
            .await
            .unwrap_err();
        assert!(
            matches!(&err, ToolError::Failed(m) if m.contains("not found") && m.contains("indentation")),
            "{err}"
        );
    }

    #[tokio::test]
    async fn consecutive_edits_work_without_rereading() {
        let (dir, c) = setup("a b c");
        read(&c).await;
        edit(&c, json!({"path": "f.rs", "old": "a", "new": "A"}))
            .await
            .unwrap();
        edit(&c, json!({"path": "f.rs", "old": "b", "new": "B"}))
            .await
            .unwrap();
        assert_eq!(file(&dir), "A B c");
    }

    #[tokio::test]
    async fn bad_inputs_are_rejected() {
        let (_dir, c) = setup("abc");
        read(&c).await;
        for input in [
            json!({"path": "f.rs", "old": "", "new": "y"}),
            json!({"path": "f.rs", "old": "a", "new": "a"}),
            json!({"path": "missing.rs", "old": "a", "new": "b"}),
            json!({"path": "f.rs", "old": "a"}),
        ] {
            let err = edit(&c, input.clone()).await.unwrap_err();
            assert!(matches!(err, ToolError::InvalidInput(_)), "{input}: {err}");
        }
    }

    #[test]
    fn snippet_numbers_lines_around_the_change() {
        let text = (1..=20)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join("\n");
        let at = text.find("10").unwrap();
        let s = snippet(&text, at, 1);
        assert!(s.starts_with("     7\t7"), "{s}");
        assert!(s.ends_with("    13\t13"), "{s}");
    }
}
