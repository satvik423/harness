//! The `grep` tool: search file contents with the ripgrep libraries (Tech spec 3.5).

use std::io;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use grep_regex::RegexMatcherBuilder;
use grep_searcher::{BinaryDetection, Searcher, SearcherBuilder, Sink, SinkContext, SinkMatch};
use harness_ports::{
    RiskCtx, Tool, ToolCtx, ToolError, ToolName, ToolOutput, ToolSpec, wrap_untrusted,
};
use harness_types::{ContentBlock, Risk};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::read_file::resolve;

/// Maximum output lines (matches plus context) collected in `content` mode.
pub const MAX_GREP_LINES: usize = 2_000;
/// Maximum context lines around each match.
const MAX_CONTEXT: usize = 10;

/// What the search reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Mode {
    /// Matching lines with line numbers (and optional context).
    Content,
    /// Only the paths of files that match.
    #[default]
    Files,
    /// Number of matching lines per file.
    Count,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    pattern: String,
    path: Option<String>,
    glob: Option<String>,
    context: Option<usize>,
    #[serde(default)]
    mode: Mode,
    #[serde(default)]
    ignore_case: bool,
}

/// Searches file contents with a regular expression (ripgrep syntax).
///
/// Respects `.gitignore` and skips binary files. Modes: `files` (default),
/// `content` and `count`.
#[derive(Debug, Clone, Copy, Default)]
pub struct Grep;

/// Collects one file's results.
struct Collect<'a> {
    shown: &'a str,
    mode: Mode,
    lines: &'a mut Vec<String>,
    count: u64,
    /// Line number of the last line written, to place `--` between groups.
    last: Option<u64>,
}

impl Collect<'_> {
    fn push(&mut self, sep: char, number: Option<u64>, bytes: &[u8]) {
        let n = number.unwrap_or(0);
        if self.last.is_some_and(|last| n > last + 1) {
            self.lines.push("--".into());
        }
        self.last = Some(n);
        let text = String::from_utf8_lossy(bytes);
        self.lines.push(format!(
            "{}{sep}{n}{sep}{}",
            self.shown,
            text.trim_end_matches(['\n', '\r'])
        ));
    }

    fn full(&self) -> bool {
        self.lines.len() >= MAX_GREP_LINES
    }
}

impl Sink for Collect<'_> {
    type Error = io::Error;

    fn matched(&mut self, _: &Searcher, m: &SinkMatch<'_>) -> Result<bool, io::Error> {
        self.count += 1;
        match self.mode {
            Mode::Content => {
                self.push(':', m.line_number(), m.bytes());
                Ok(!self.full())
            }
            // One match is enough to list the file.
            Mode::Files => Ok(false),
            Mode::Count => Ok(true),
        }
    }

    fn context(&mut self, _: &Searcher, c: &SinkContext<'_>) -> Result<bool, io::Error> {
        if self.mode == Mode::Content {
            self.push('-', c.line_number(), c.bytes());
        }
        Ok(!self.full())
    }
}

/// The search result, ready to format.
struct Found {
    lines: Vec<String>,
    files: usize,
    truncated: bool,
}

fn search(cwd: &Path, root: &Path, input: &Input) -> Result<Found, ToolError> {
    let matcher = RegexMatcherBuilder::new()
        .case_insensitive(input.ignore_case)
        .build(&input.pattern)
        .map_err(|e| ToolError::InvalidInput(format!("bad regex: {e}")))?;
    let context = input.context.unwrap_or(0).min(MAX_CONTEXT);
    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(context)
        .after_context(context)
        .binary_detection(BinaryDetection::quit(0))
        .build();
    let mut walk = ignore::WalkBuilder::new(root);
    if let Some(glob) = &input.glob {
        let overrides = ignore::overrides::OverrideBuilder::new(root)
            .add(glob)
            .and_then(|b| b.build())
            .map_err(|e| ToolError::InvalidInput(format!("bad glob: {e}")))?;
        walk.overrides(overrides);
    }
    let mut files: Vec<PathBuf> = walk
        .build()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_some_and(|t| t.is_file()))
        .map(ignore::DirEntry::into_path)
        .collect();
    files.sort();

    let mut lines = Vec::new();
    let mut matched_files = 0;
    let mut truncated = false;
    for file in files {
        if lines.len() >= MAX_GREP_LINES {
            truncated = true;
            break;
        }
        let shown = file
            .strip_prefix(cwd)
            .unwrap_or(&file)
            .to_string_lossy()
            .into_owned();
        let mut sink = Collect {
            shown: &shown,
            mode: input.mode,
            lines: &mut lines,
            count: 0,
            last: None,
        };
        // Unreadable files are skipped, as ripgrep does.
        if searcher.search_path(&matcher, &file, &mut sink).is_err() {
            continue;
        }
        let count = sink.count;
        if count == 0 {
            continue;
        }
        matched_files += 1;
        match input.mode {
            Mode::Content => {
                if lines.len() >= MAX_GREP_LINES {
                    truncated = true;
                }
            }
            Mode::Files => lines.push(shown),
            Mode::Count => lines.push(format!("{shown}:{count}")),
        }
    }
    Ok(Found {
        lines,
        files: matched_files,
        truncated,
    })
}

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: ToolName::new("grep").unwrap_or_else(|_| unreachable!("valid literal")),
            description: "Search file contents with a regular expression (ripgrep syntax). \
                Respects .gitignore and skips binary files. mode: `files` (default) lists \
                matching files, `content` shows matching lines as path:line:text (with \
                `context` lines around them), `count` gives matches per file. Use `glob` \
                to filter files, for example `*.rs`."
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Regular expression."},
                    "path": {"type": "string", "description": "File or directory to search (default: the working directory)."},
                    "glob": {"type": "string", "description": "Only search files matching this glob."},
                    "context": {"type": "integer", "minimum": 0, "maximum": 10, "description": "Lines of context around matches (content mode)."},
                    "mode": {"type": "string", "enum": ["content", "files", "count"]},
                    "ignore_case": {"type": "boolean"}
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
        let root = input
            .path
            .as_deref()
            .map_or_else(|| ctx.cwd.clone(), |p| resolve(&ctx.cwd, p));
        if !root.exists() {
            return Err(ToolError::InvalidInput(format!(
                "{} does not exist",
                input.path.as_deref().unwrap_or(".")
            )));
        }
        let cwd = ctx.cwd.clone();
        let pattern = input.pattern.clone();
        let mode = input.mode;
        // Searching many files is blocking file-system work.
        let found = tokio::task::spawn_blocking(move || search(&cwd, &root, &input))
            .await
            .map_err(|e| ToolError::Failed(format!("search stopped: {e}")))??;
        let mut text = if found.lines.is_empty() {
            format!("No matches for {pattern}.")
        } else if mode == Mode::Content {
            // Matching lines are file contents: data, never instructions (guardrail 12).
            wrap_untrusted(&format!("grep:{pattern}"), &found.lines.join("\n"))
        } else {
            found.lines.join("\n")
        };
        if found.truncated {
            text.push_str(&format!(
                "\n[stopped after {MAX_GREP_LINES} lines; narrow the pattern, path or glob]"
            ));
        }
        Ok(ToolOutput {
            content: vec![ContentBlock::text(text)],
            summary: format!("{} files match {pattern}", found.files),
            touched: Vec::new(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::ctx;

    /// The body of an `<untrusted>` wrapper, or the text itself.
    fn unwrap_untrusted(text: &str) -> String {
        match text.strip_prefix("<untrusted source=\"") {
            Some(rest) => {
                let body = rest.split_once("\">\n").unwrap().1;
                let (body, note) = body.split_once("\n</untrusted>").unwrap();
                format!("{body}{note}")
            }
            None => text.to_owned(),
        }
    }

    #[tokio::test]
    async fn content_mode_output_is_wrapped_as_untrusted() {
        let dir = project();
        let (c, _, _) = ctx(dir.path());
        let out = Grep
            .run(json!({"pattern": "beta", "mode": "content"}), c)
            .await
            .unwrap();
        assert_eq!(
            out.content,
            vec![ContentBlock::text(
                "<untrusted source=\"grep:beta\">\nsrc/a.rs:2:fn beta() {}\n</untrusted>"
            )]
        );
    }

    fn project() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path();
        std::fs::create_dir_all(p.join("src")).unwrap();
        std::fs::write(
            p.join("src/a.rs"),
            "fn alpha() {}\nfn beta() {}\n// TODO: gamma\n",
        )
        .unwrap();
        std::fs::write(p.join("src/b.py"), "def alpha():\n    pass\n").unwrap();
        std::fs::write(p.join("notes.txt"), "nothing here\n").unwrap();
        dir
    }

    async fn grep(dir: &Path, input: Value) -> Result<String, ToolError> {
        let (c, _, _) = ctx(dir);
        let out = Grep.run(input, c).await?;
        match out.content.as_slice() {
            [ContentBlock::Text { text }] => Ok(unwrap_untrusted(text)),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[tokio::test]
    async fn files_mode_is_the_default_and_lists_matching_files() {
        let dir = project();
        let out = grep(dir.path(), json!({"pattern": "alpha"})).await.unwrap();
        assert_eq!(out, "src/a.rs\nsrc/b.py");
    }

    #[tokio::test]
    async fn content_mode_shows_numbered_lines() {
        let dir = project();
        let out = grep(
            dir.path(),
            json!({"pattern": "beta|gamma", "mode": "content"}),
        )
        .await
        .unwrap();
        assert_eq!(out, "src/a.rs:2:fn beta() {}\nsrc/a.rs:3:// TODO: gamma");
    }

    #[tokio::test]
    async fn content_mode_with_context_marks_breaks() {
        let dir = tempfile::tempdir().unwrap();
        let body: String = (1..=20).map(|i| format!("l{i}\n")).collect();
        std::fs::write(dir.path().join("f.txt"), body).unwrap();
        let out = grep(
            dir.path(),
            json!({"pattern": "^l(3|15)$", "mode": "content", "context": 1}),
        )
        .await
        .unwrap();
        assert_eq!(
            out,
            "f.txt-2-l2\nf.txt:3:l3\nf.txt-4-l4\n--\nf.txt-14-l14\nf.txt:15:l15\nf.txt-16-l16"
        );
    }

    #[tokio::test]
    async fn count_mode_counts_per_file() {
        let dir = project();
        let out = grep(dir.path(), json!({"pattern": "fn ", "mode": "count"}))
            .await
            .unwrap();
        assert_eq!(out, "src/a.rs:2");
    }

    #[tokio::test]
    async fn glob_filters_files() {
        let dir = project();
        let out = grep(dir.path(), json!({"pattern": "alpha", "glob": "*.py"}))
            .await
            .unwrap();
        assert_eq!(out, "src/b.py");
    }

    #[tokio::test]
    async fn ignore_case_and_single_file_path() {
        let dir = project();
        let out = grep(
            dir.path(),
            json!({"pattern": "ALPHA", "ignore_case": true, "path": "src/a.rs", "mode": "content"}),
        )
        .await
        .unwrap();
        assert_eq!(out, "src/a.rs:1:fn alpha() {}");
    }

    #[tokio::test]
    async fn binary_and_ignored_files_are_skipped() {
        let dir = project();
        std::fs::create_dir(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join(".gitignore"), "gen/\n").unwrap();
        std::fs::create_dir(dir.path().join("gen")).unwrap();
        std::fs::write(dir.path().join("gen/x.rs"), "alpha").unwrap();
        std::fs::write(dir.path().join("blob.bin"), b"alpha\0\x01\x02").unwrap();
        let out = grep(dir.path(), json!({"pattern": "alpha"})).await.unwrap();
        assert_eq!(out, "src/a.rs\nsrc/b.py");
    }

    #[tokio::test]
    async fn no_match_says_so() {
        let dir = project();
        assert_eq!(
            grep(dir.path(), json!({"pattern": "zeta"})).await.unwrap(),
            "No matches for zeta."
        );
    }

    #[tokio::test]
    async fn bad_regex_glob_or_path_is_invalid_input() {
        let dir = project();
        for input in [
            json!({"pattern": "("}),
            json!({"pattern": "a", "glob": "[" }),
            json!({"pattern": "a", "path": "missing"}),
            json!({"pattern": "a", "mode": "lines"}),
        ] {
            assert!(
                matches!(
                    grep(dir.path(), input.clone()).await,
                    Err(ToolError::InvalidInput(_))
                ),
                "{input}"
            );
        }
    }

    #[tokio::test]
    async fn content_output_stops_at_the_line_limit() {
        let dir = tempfile::tempdir().unwrap();
        let body = "hit\n".repeat(MAX_GREP_LINES + 50);
        std::fs::write(dir.path().join("big.txt"), body).unwrap();
        let out = grep(dir.path(), json!({"pattern": "hit", "mode": "content"}))
            .await
            .unwrap();
        assert_eq!(out.lines().count(), MAX_GREP_LINES + 1);
        assert!(out.ends_with("narrow the pattern, path or glob]"));
    }
}
