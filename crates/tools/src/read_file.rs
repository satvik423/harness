//! The `read_file` tool: read a text file with line numbers (Tech spec 3.5).

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use harness_ports::{RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::support::stamp_of;

/// Maximum lines returned by one call.
pub const MAX_LINES: usize = 2_000;
/// Lines longer than this many characters are truncated.
pub const MAX_LINE_CHARS: usize = 2_000;
/// Bytes inspected for a NUL byte when deciding whether a file is binary.
const BINARY_SNIFF_BYTES: usize = 8_000;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    path: String,
    offset: Option<usize>,
    limit: Option<usize>,
}

/// Reads a text file and returns numbered lines (`cat -n` style).
///
/// Every successful read is recorded in the session's read ledger, which the
/// edit tools (Phase 2) check before changing a file.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadFile;

/// Resolves `path` against `cwd` (absolute paths are used as given).
pub(crate) fn resolve(cwd: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        cwd.join(p)
    }
}

/// Formats the selected lines of `text` (1-based `offset`, at most `limit` lines).
///
/// # Errors
/// [`ToolError::InvalidInput`] if `offset` is past the end of a non-empty file.
fn render(text: &str, offset: usize, limit: usize) -> Result<String, ToolError> {
    if text.is_empty() {
        return Ok("(empty file)".into());
    }
    let lines: Vec<&str> = text.lines().collect();
    let total = lines.len();
    let start = offset.max(1) - 1;
    if start >= total {
        return Err(ToolError::InvalidInput(format!(
            "offset {offset} is past the end of the file ({total} lines)"
        )));
    }
    let end = start.saturating_add(limit).min(total);
    let mut out = lines[start..end]
        .iter()
        .enumerate()
        .map(|(i, line)| format!("{:>6}\t{}", start + i + 1, truncate_line(line)))
        .collect::<Vec<_>>()
        .join("\n");
    if end < total {
        out.push_str(&format!(
            "\n[showing lines {}-{end} of {total}; use offset to read more]",
            start + 1
        ));
    }
    Ok(out)
}

/// Cuts a line to [`MAX_LINE_CHARS`] characters, marking the cut.
fn truncate_line(line: &str) -> String {
    if line.chars().count() <= MAX_LINE_CHARS {
        line.to_owned()
    } else {
        let head: String = line.chars().take(MAX_LINE_CHARS).collect();
        format!("{head} [line truncated]")
    }
}

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("read_file").unwrap_or_else(|_| unreachable!("valid literal")),
            description: format!(
                "Read a text file. Returns lines prefixed with their line numbers. Reads up to {MAX_LINES} lines \
                 per call; use offset (1-based) and limit to page through longer files. Relative paths are \
                 resolved against the working directory."
            ),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path, absolute or relative to the working directory."},
                    "offset": {"type": "integer", "minimum": 1, "description": "First line to read (1-based). Default 1."},
                    "limit": {"type": "integer", "minimum": 1, "maximum": MAX_LINES, "description": "Number of lines. Default and maximum 2000."}
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            strict: true,
            eager_input_streaming: false,
        }
    }

    fn risk(&self, input: &Value, ctx: &RiskCtx) -> Risk {
        let paths = input["path"]
            .as_str()
            .map(|p| resolve(&ctx.cwd, p))
            .into_iter()
            .collect();
        Risk::ReadOnly { paths }
    }

    fn parallel_safe(&self) -> bool {
        true
    }

    async fn run(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput(e.to_string()))?;
        let offset = input.offset.unwrap_or(1);
        let limit = input.limit.unwrap_or(MAX_LINES).min(MAX_LINES);
        if offset == 0 || limit == 0 {
            return Err(ToolError::InvalidInput(
                "offset and limit must be at least 1".into(),
            ));
        }
        let path = resolve(&ctx.cwd, &input.path);
        let shown = path.display();
        let meta = tokio::fs::metadata(&path)
            .await
            .map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => ToolError::Failed(format!("no such file: {shown}")),
                _ => ToolError::Failed(format!("cannot read {shown}: {e}")),
            })?;
        if meta.is_dir() {
            return Err(ToolError::Failed(format!(
                "{shown} is a directory; use the shell tool to list it"
            )));
        }
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(|e| ToolError::Failed(format!("cannot read {shown}: {e}")))?;
        if bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0) {
            return Err(ToolError::Refused(format!(
                "{shown} looks like a binary file"
            )));
        }
        let text = String::from_utf8_lossy(&bytes);
        let body = render(&text, offset, limit)?;
        ctx.ledger.record(&path, stamp_of(&bytes, &meta));
        Ok(ToolOutput {
            summary: format!("read {} ({} lines)", input.path, text.lines().count()),
            content: vec![ContentBlock::text(body)],
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;
    use harness_ports::{FileStamp, LedgerCheck, ReadLedger};

    async fn read(dir: &Path, input: Value) -> Result<String, ToolError> {
        let (ctx, _, _) = ctx(dir);
        let out = ReadFile.run(input, ctx).await?;
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => Ok(text.clone()),
            other => panic!("unexpected content {other:?}"),
        }
    }

    #[test]
    fn render_numbers_lines_from_offset() {
        let out = render("a\nb\nc\n", 2, 10).unwrap();
        assert_eq!(out, "     2\tb\n     3\tc");
    }

    #[test]
    fn render_reports_more_lines_beyond_limit() {
        let out = render("a\nb\nc", 1, 2).unwrap();
        assert!(out.starts_with("     1\ta\n     2\tb"), "{out}");
        assert!(
            out.contains("[showing lines 1-2 of 3; use offset to read more]"),
            "{out}"
        );
    }

    #[test]
    fn render_truncates_very_long_lines() {
        let long = "x".repeat(MAX_LINE_CHARS + 5);
        let out = render(&long, 1, 10).unwrap();
        assert!(
            out.ends_with("[line truncated]"),
            "{}",
            &out[out.len() - 40..]
        );
        assert!(out.len() < MAX_LINE_CHARS + 40);
    }

    #[test]
    fn render_rejects_offset_past_end() {
        assert!(matches!(
            render("a\nb", 5, 10),
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[test]
    fn render_handles_empty_file() {
        assert_eq!(render("", 1, 10).unwrap(), "(empty file)");
    }

    #[tokio::test]
    async fn reads_relative_paths_and_records_the_ledger() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.txt"), "hello\nworld\n").unwrap();
        let (ctx, _, ledger) = ctx(dir.path());
        let out = ReadFile.run(json!({"path": "a.txt"}), ctx).await.unwrap();
        assert_eq!(
            out.content,
            vec![ContentBlock::text("     1\thello\n     2\tworld")]
        );
        let meta = std::fs::metadata(dir.path().join("a.txt")).unwrap();
        let stamp: FileStamp = stamp_of(b"hello\nworld\n", &meta);
        assert_eq!(
            ledger.check(&dir.path().join("a.txt"), &stamp),
            LedgerCheck::Fresh
        );
    }

    #[tokio::test]
    async fn missing_file_and_directory_fail_clearly() {
        let dir = tempfile::tempdir().unwrap();
        let err = read(dir.path(), json!({"path": "nope.txt"}))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("no such file"), "{err}");
        let err = read(dir.path(), json!({"path": "."})).await.unwrap_err();
        assert!(err.to_string().contains("directory"), "{err}");
    }

    #[tokio::test]
    async fn binary_files_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.bin"), [0u8, 1, 2, 0]).unwrap();
        assert!(matches!(
            read(dir.path(), json!({"path": "b.bin"})).await,
            Err(ToolError::Refused(_))
        ));
    }

    #[tokio::test]
    async fn unknown_fields_are_invalid_input() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            read(dir.path(), json!({"path": "x", "bogus": 1})).await,
            Err(ToolError::InvalidInput(_))
        ));
    }

    #[test]
    fn risk_is_read_only_with_resolved_path() {
        let ctx = RiskCtx {
            cwd: "/proj".into(),
            writable_roots: vec![],
        };
        assert_eq!(
            ReadFile.risk(&json!({"path": "src/a.rs"}), &ctx),
            Risk::ReadOnly {
                paths: vec!["/proj/src/a.rs".into()]
            }
        );
        assert!(ReadFile.parallel_safe());
        assert!(ReadFile.spec().strict);
    }
}
