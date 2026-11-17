//! Parser for the `apply_patch` format (Tech spec 3.5, Codex-compatible).
//!
//! ```text
//! patch  := "*** Begin Patch\n" op+ "*** End Patch"
//! op     := add | delete | update
//! add    := "*** Add File: " path "\n" ("+" line "\n")*
//! delete := "*** Delete File: " path "\n"
//! update := "*** Update File: " path "\n" [ "*** Move to: " path "\n" ] hunk+
//! hunk   := "@@" [ " " anchor_text ] "\n" ( (" " | "-" | "+") line "\n" )+
//! ```
//!
//! Two leniencies, both common in model output: an empty line inside a hunk is
//! an empty context line, and a `*** End of File` line marks a hunk that must
//! match at the end of the file. Blank lines around the patch and `\r\n` line
//! endings (when the whole patch uses them) are accepted.

use std::fmt;

const BEGIN: &str = "*** Begin Patch";
const END: &str = "*** End Patch";
const ADD: &str = "*** Add File: ";
const DELETE: &str = "*** Delete File: ";
const UPDATE: &str = "*** Update File: ";
const MOVE: &str = "*** Move to: ";
const END_OF_FILE: &str = "*** End of File";

/// A parsed patch: one or more file operations, applied in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Patch {
    /// The operations, in the order they appear.
    pub ops: Vec<PatchOp>,
}

/// One file operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatchOp {
    /// Create a file with these lines.
    Add {
        /// Path relative to the working directory.
        path: String,
        /// The file's lines, without line endings.
        lines: Vec<String>,
    },
    /// Delete a file.
    Delete {
        /// Path relative to the working directory.
        path: String,
    },
    /// Change a file, and optionally rename it.
    Update {
        /// Path relative to the working directory.
        path: String,
        /// New path, if the file is also renamed.
        move_to: Option<String>,
        /// Changes, in file order.
        hunks: Vec<Hunk>,
    },
}

/// One block of changes in an updated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// Text of a line at or before the change, used to find it (`@@ anchor`).
    pub anchor: Option<String>,
    /// Context, removed and added lines, in order.
    pub lines: Vec<HunkLine>,
    /// The hunk must match at the end of the file (`*** End of File`).
    pub at_eof: bool,
}

/// One line of a hunk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    /// Unchanged line (` `), used to locate the change.
    Context(String),
    /// Line to remove (`-`).
    Remove(String),
    /// Line to add (`+`).
    Add(String),
}

/// Why a patch could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    /// 1-based line number in the patch text.
    pub line: usize,
    /// What is wrong.
    pub message: String,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "patch line {}: {}", self.line, self.message)
    }
}

impl std::error::Error for ParseError {}

impl Patch {
    /// Paths named by the patch, including rename targets, in order.
    #[must_use]
    pub fn paths(&self) -> Vec<&str> {
        let mut out = Vec::new();
        for op in &self.ops {
            match op {
                PatchOp::Add { path, .. } | PatchOp::Delete { path } => out.push(path.as_str()),
                PatchOp::Update { path, move_to, .. } => {
                    out.push(path.as_str());
                    if let Some(to) = move_to {
                        out.push(to.as_str());
                    }
                }
            }
        }
        out
    }

    /// Renders the patch back to text; [`parse_patch`] of the result is `self`.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = format!("{BEGIN}\n");
        for op in &self.ops {
            match op {
                PatchOp::Add { path, lines } => {
                    out.push_str(&format!("{ADD}{path}\n"));
                    for l in lines {
                        out.push_str(&format!("+{l}\n"));
                    }
                }
                PatchOp::Delete { path } => out.push_str(&format!("{DELETE}{path}\n")),
                PatchOp::Update {
                    path,
                    move_to,
                    hunks,
                } => {
                    out.push_str(&format!("{UPDATE}{path}\n"));
                    if let Some(to) = move_to {
                        out.push_str(&format!("{MOVE}{to}\n"));
                    }
                    for h in hunks {
                        match &h.anchor {
                            Some(a) => out.push_str(&format!("@@ {a}\n")),
                            None => out.push_str("@@\n"),
                        }
                        for l in &h.lines {
                            let (c, t) = match l {
                                HunkLine::Context(t) => (' ', t),
                                HunkLine::Remove(t) => ('-', t),
                                HunkLine::Add(t) => ('+', t),
                            };
                            out.push(c);
                            out.push_str(t);
                            out.push('\n');
                        }
                        if h.at_eof {
                            out.push_str(&format!("{END_OF_FILE}\n"));
                        }
                    }
                }
            }
        }
        out.push_str(END);
        out
    }
}

/// Parses patch text.
///
/// # Errors
/// [`ParseError`] naming the first line that does not fit the grammar.
///
/// # Examples
/// ```
/// use harness_tools::patch::{parse_patch, PatchOp};
/// let p = parse_patch("*** Begin Patch\n*** Delete File: old.txt\n*** End Patch").unwrap();
/// assert_eq!(p.ops, [PatchOp::Delete { path: "old.txt".into() }]);
/// ```
pub fn parse_patch(text: &str) -> Result<Patch, ParseError> {
    // A CRLF patch (judged by its first line) has one `\r` stripped per line. In
    // an LF patch a `\r` is line content, so render() and parse_patch() round-trip.
    let crlf = text
        .trim_start_matches(['\r', '\n'])
        .split('\n')
        .next()
        .is_some_and(|l| l.ends_with('\r'));
    let lines: Vec<&str> = text
        .split('\n')
        .map(|l| {
            if crlf {
                l.strip_suffix('\r').unwrap_or(l)
            } else {
                l
            }
        })
        .collect();
    let err = |i: usize, message: String| ParseError {
        line: i + 1,
        message,
    };
    let first = lines
        .iter()
        .position(|l| !l.trim().is_empty())
        .ok_or_else(|| err(0, "the patch is empty".into()))?;
    if lines[first].trim_end() != BEGIN {
        return Err(err(first, format!("expected `{BEGIN}`")));
    }
    let last = lines
        .iter()
        .rposition(|l| !l.trim().is_empty())
        .unwrap_or(first);
    if last == first || lines[last].trim_end() != END {
        return Err(err(last, format!("expected `{END}` as the last line")));
    }

    let mut ops = Vec::new();
    let mut i = first + 1;
    while i < last {
        let line = lines[i];
        if let Some(path) = line.strip_prefix(ADD) {
            let path = check_path(path, i)?;
            i += 1;
            let mut body = Vec::new();
            while i < last && !is_header(lines[i]) {
                let l = lines[i]
                    .strip_prefix('+')
                    .ok_or_else(|| err(i, "lines of an added file must start with `+`".into()))?;
                body.push(l.to_owned());
                i += 1;
            }
            ops.push(PatchOp::Add { path, lines: body });
        } else if let Some(path) = line.strip_prefix(DELETE) {
            ops.push(PatchOp::Delete {
                path: check_path(path, i)?,
            });
            i += 1;
        } else if let Some(path) = line.strip_prefix(UPDATE) {
            let path = check_path(path, i)?;
            let header = i;
            i += 1;
            let mut move_to = None;
            if i < last
                && let Some(to) = lines[i].strip_prefix(MOVE)
            {
                move_to = Some(check_path(to, i)?);
                i += 1;
            }
            let mut hunks = Vec::new();
            while i < last && !is_header(lines[i]) {
                let (hunk, next) = parse_hunk(&lines, i, last)?;
                hunks.push(hunk);
                i = next;
            }
            if hunks.is_empty() {
                return Err(err(header, format!("`{UPDATE}{path}` has no `@@` hunk")));
            }
            ops.push(PatchOp::Update {
                path,
                move_to,
                hunks,
            });
        } else if line.trim().is_empty() {
            i += 1;
        } else {
            return Err(err(
                i,
                format!("expected `{ADD}`, `{DELETE}` or `{UPDATE}`, found {line:?}"),
            ));
        }
    }
    if ops.is_empty() {
        return Err(err(first, "the patch has no file operations".into()));
    }
    Ok(Patch { ops })
}

/// A line starting a new file operation (ends the current one).
fn is_header(line: &str) -> bool {
    line.starts_with(ADD) || line.starts_with(DELETE) || line.starts_with(UPDATE)
}

/// Parses one hunk starting at its `@@` line; returns it and the next line index.
fn parse_hunk(lines: &[&str], start: usize, last: usize) -> Result<(Hunk, usize), ParseError> {
    let err = |i: usize, message: String| ParseError {
        line: i + 1,
        message,
    };
    let head = lines[start];
    let anchor = match head.strip_prefix("@@") {
        Some(rest) if rest.trim().is_empty() => None,
        Some(rest) => Some(rest.strip_prefix(' ').unwrap_or(rest).trim_end().to_owned()),
        None => {
            return Err(err(
                start,
                format!("expected a `@@` hunk header, found {head:?}"),
            ));
        }
    };
    let mut body = Vec::new();
    let mut at_eof = false;
    let mut i = start + 1;
    while i < last && !is_header(lines[i]) && !lines[i].starts_with("@@") {
        let line = lines[i];
        if line.trim_end() == END_OF_FILE {
            at_eof = true;
            i += 1;
            break;
        }
        let parsed = match line.chars().next() {
            Some(' ') => HunkLine::Context(line[1..].to_owned()),
            Some('-') => HunkLine::Remove(line[1..].to_owned()),
            Some('+') => HunkLine::Add(line[1..].to_owned()),
            None => HunkLine::Context(String::new()),
            Some(_) => {
                return Err(err(
                    i,
                    format!("hunk lines must start with ' ', '-' or '+', found {line:?}"),
                ));
            }
        };
        body.push(parsed);
        i += 1;
    }
    // Trailing empty lines are usually blank lines before the next header, not context.
    while matches!(body.last(), Some(HunkLine::Context(t)) if t.is_empty()) {
        body.pop();
    }
    if !body
        .iter()
        .any(|l| matches!(l, HunkLine::Add(_) | HunkLine::Remove(_)))
    {
        return Err(err(
            start,
            "the hunk changes nothing (no `+` or `-` lines)".into(),
        ));
    }
    Ok((
        Hunk {
            anchor,
            lines: body,
            at_eof,
        },
        i,
    ))
}

/// Checks a path from a header: non-empty, relative, no NUL.
fn check_path(raw: &str, i: usize) -> Result<String, ParseError> {
    let path = raw.trim();
    let problem = if path.is_empty() {
        Some("the path is empty")
    } else if path.starts_with('/') {
        Some("paths must be relative to the working directory")
    } else if path.contains('\0') {
        Some("the path contains a NUL byte")
    } else {
        None
    };
    match problem {
        Some(message) => Err(ParseError {
            line: i + 1,
            message: message.into(),
        }),
        None => Ok(path.to_owned()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(s: &str) -> HunkLine {
        HunkLine::Context(s.into())
    }
    fn rm(s: &str) -> HunkLine {
        HunkLine::Remove(s.into())
    }
    fn add(s: &str) -> HunkLine {
        HunkLine::Add(s.into())
    }

    #[test]
    fn parses_every_operation() {
        let text = "\
*** Begin Patch
*** Add File: src/new.rs
+fn main() {}
+
*** Delete File: old.txt
*** Update File: src/lib.rs
*** Move to: src/core.rs
@@ fn alpha()
 let a = 1;
-let b = 2;
+let b = 3;
@@
-x
+y
*** End of File
*** End Patch
";
        let p = parse_patch(text).unwrap();
        assert_eq!(
            p.ops,
            vec![
                PatchOp::Add {
                    path: "src/new.rs".into(),
                    lines: vec!["fn main() {}".into(), String::new()],
                },
                PatchOp::Delete {
                    path: "old.txt".into()
                },
                PatchOp::Update {
                    path: "src/lib.rs".into(),
                    move_to: Some("src/core.rs".into()),
                    hunks: vec![
                        Hunk {
                            anchor: Some("fn alpha()".into()),
                            lines: vec![ctx("let a = 1;"), rm("let b = 2;"), add("let b = 3;")],
                            at_eof: false,
                        },
                        Hunk {
                            anchor: None,
                            lines: vec![rm("x"), add("y")],
                            at_eof: true,
                        },
                    ],
                },
            ]
        );
        assert_eq!(
            p.paths(),
            ["src/new.rs", "old.txt", "src/lib.rs", "src/core.rs"]
        );
    }

    #[test]
    fn accepts_crlf_and_surrounding_blank_lines() {
        let text = "\r\n*** Begin Patch\r\n*** Delete File: a\r\n*** End Patch\r\n\r\n";
        assert_eq!(
            parse_patch(text).unwrap().ops,
            [PatchOp::Delete { path: "a".into() }]
        );
    }

    #[test]
    fn a_carriage_return_in_an_lf_patch_is_line_content() {
        // Found by the fuzzer: stripping `\r` from every line lost one per round trip.
        let text = "*** Begin Patch\n*** Update File: a\n@@\n-y\r\r\n+z\n*** End Patch";
        let p = parse_patch(text).unwrap();
        let PatchOp::Update { hunks, .. } = &p.ops[0] else {
            panic!()
        };
        assert_eq!(hunks[0].lines, [rm("y\r\r"), add("z")]);
        assert_eq!(parse_patch(&p.render()).unwrap(), p);
    }

    #[test]
    fn an_empty_line_in_a_hunk_is_empty_context() {
        let text = "*** Begin Patch\n*** Update File: a\n@@\n a\n\n-b\n*** End Patch";
        let PatchOp::Update { hunks, .. } = &parse_patch(text).unwrap().ops[0] else {
            panic!()
        };
        assert_eq!(hunks[0].lines, [ctx("a"), ctx(""), rm("b")]);
    }

    #[test]
    fn rejects_malformed_patches_with_the_line_number() {
        let cases = [
            ("", 1, "empty"),
            ("hello", 1, "Begin Patch"),
            ("*** Begin Patch\n*** Delete File: a", 2, "End Patch"),
            ("*** Begin Patch\n*** End Patch", 1, "no file operations"),
            ("*** Begin Patch\nnonsense\n*** End Patch", 2, "expected"),
            (
                "*** Begin Patch\n*** Add File: a\nno plus\n*** End Patch",
                3,
                "`+`",
            ),
            (
                "*** Begin Patch\n*** Update File: a\n*** End Patch",
                2,
                "no `@@`",
            ),
            (
                "*** Begin Patch\n*** Update File: a\n@@\n context\n*** End Patch",
                3,
                "changes nothing",
            ),
            (
                "*** Begin Patch\n*** Update File: a\n@@\n*bad\n*** End Patch",
                4,
                "must start with",
            ),
            (
                "*** Begin Patch\n*** Delete File: /etc/passwd\n*** End Patch",
                2,
                "relative",
            ),
            (
                "*** Begin Patch\n*** Delete File: \n*** End Patch",
                2,
                "empty",
            ),
        ];
        for (text, line, needle) in cases {
            let e = parse_patch(text).unwrap_err();
            assert_eq!(e.line, line, "{text:?}: {e}");
            assert!(e.message.contains(needle), "{text:?}: {e}");
        }
    }

    #[test]
    fn render_round_trips() {
        let text = "*** Begin Patch\n*** Update File: a.rs\n@@ fn f()\n x\n-y\n+z\n*** End of File\n*** Add File: b\n+1\n*** End Patch";
        let p = parse_patch(text).unwrap();
        assert_eq!(p.render(), text);
        assert_eq!(parse_patch(&p.render()).unwrap(), p);
    }
}
