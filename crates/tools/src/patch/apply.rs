//! Planning a patch against file contents, without touching the disk.
//!
//! [`plan`] works on a [`PatchFs`] view, so it can be tested and fuzzed in
//! memory. It either produces every final file state, or fails with nothing
//! applied: the all-or-nothing rule. Writing the plan to disk is the tool's job.

use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path};

use super::parse::{Hunk, HunkLine, Patch, PatchOp};

/// Read access to current file contents, for [`plan`].
pub trait PatchFs {
    /// The contents of `path` (relative to the working directory), or `None`
    /// if there is no such file.
    ///
    /// # Errors
    /// A message if the file exists but cannot be used (for example it is not UTF-8).
    fn read(&self, path: &str) -> Result<Option<String>, String>;
}

impl PatchFs for HashMap<String, String> {
    fn read(&self, path: &str) -> Result<Option<String>, String> {
        Ok(self.get(path).cloned())
    }
}

/// The final state of one file after the patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChange {
    /// Normalised path relative to the working directory.
    pub path: String,
    /// Contents before the patch (`None`: the file did not exist).
    pub before: Option<String>,
    /// Contents after the patch (`None`: the file is deleted).
    pub after: Option<String>,
}

/// Why a patch cannot be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanError(pub String);

impl fmt::Display for PlanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for PlanError {}

/// Lexically normalises a relative path: drops `.` components and empty parts.
///
/// # Examples
/// ```
/// use harness_tools::patch::normalize;
/// assert_eq!(normalize("./src//a.rs"), "src/a.rs");
/// ```
#[must_use]
pub fn normalize(path: &str) -> String {
    Path::new(path)
        .components()
        .filter(|c| !matches!(c, Component::CurDir))
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Computes the final contents of every file the patch touches.
///
/// Operations apply in order, so a later operation sees the result of an
/// earlier one on the same file. Each hunk must be found exactly once, after
/// the previous hunk of the same file: first by exact match, then ignoring
/// trailing whitespace, then ignoring all surrounding whitespace.
///
/// # Errors
/// [`PlanError`] describing the first problem (missing file, existing file for
/// an add, hunk not found or ambiguous); nothing is applied in that case.
///
/// # Examples
/// ```
/// use std::collections::HashMap;
/// use harness_tools::patch::{parse_patch, plan};
/// let fs = HashMap::from([("a.txt".to_string(), "one\ntwo\n".to_string())]);
/// let patch = parse_patch("*** Begin Patch\n*** Update File: a.txt\n@@\n-two\n+2\n*** End Patch").unwrap();
/// let changes = plan(&patch, &fs).unwrap();
/// assert_eq!(changes[0].after.as_deref(), Some("one\n2\n"));
/// ```
pub fn plan(patch: &Patch, fs: &dyn PatchFs) -> Result<Vec<FileChange>, PlanError> {
    let mut state = State {
        fs,
        order: Vec::new(),
        before: HashMap::new(),
        now: HashMap::new(),
    };
    for op in &patch.ops {
        match op {
            PatchOp::Add { path, lines } => {
                let path = normalize(path);
                if state.get(&path)?.is_some() {
                    return Err(PlanError(format!(
                        "cannot add {path}: it already exists (use *** Update File)"
                    )));
                }
                let mut text = lines.join("\n");
                if !lines.is_empty() {
                    text.push('\n');
                }
                state.set(&path, Some(text));
            }
            PatchOp::Delete { path } => {
                let path = normalize(path);
                if state.get(&path)?.is_none() {
                    return Err(PlanError(format!("cannot delete {path}: no such file")));
                }
                state.set(&path, None);
            }
            PatchOp::Update {
                path,
                move_to,
                hunks,
            } => {
                let path = normalize(path);
                let Some(text) = state.get(&path)? else {
                    return Err(PlanError(format!("cannot update {path}: no such file")));
                };
                let updated = apply_hunks(&path, &text, hunks)?;
                match move_to.as_deref().map(normalize) {
                    Some(to) if to != path => {
                        if state.get(&to)?.is_some() {
                            return Err(PlanError(format!(
                                "cannot move {path} to {to}: {to} already exists"
                            )));
                        }
                        state.set(&path, None);
                        state.set(&to, Some(updated));
                    }
                    _ => state.set(&path, Some(updated)),
                }
            }
        }
    }
    Ok(state
        .order
        .iter()
        .filter_map(|path| {
            let before = state.before.get(path).cloned().flatten();
            let after = state.now.get(path).cloned().flatten();
            (before != after).then(|| FileChange {
                path: path.clone(),
                before,
                after,
            })
        })
        .collect())
}

/// The planner's overlay of patched files over the real ones.
struct State<'a> {
    fs: &'a dyn PatchFs,
    order: Vec<String>,
    before: HashMap<String, Option<String>>,
    now: HashMap<String, Option<String>>,
}

impl State<'_> {
    fn get(&mut self, path: &str) -> Result<Option<String>, PlanError> {
        if let Some(now) = self.now.get(path) {
            return Ok(now.clone());
        }
        let current = self
            .fs
            .read(path)
            .map_err(|e| PlanError(format!("cannot use {path}: {e}")))?;
        self.remember(path, current.clone());
        Ok(current)
    }

    fn set(&mut self, path: &str, contents: Option<String>) {
        if !self.before.contains_key(path) {
            // Only reached for paths that `get` has not seen; every op calls
            // `get` first except an add's target, which `get` checked too.
            let current = self.fs.read(path).ok().flatten();
            self.remember(path, current);
        }
        self.now.insert(path.to_owned(), contents);
    }

    fn remember(&mut self, path: &str, current: Option<String>) {
        if !self.before.contains_key(path) {
            self.order.push(path.to_owned());
            self.before.insert(path.to_owned(), current.clone());
            self.now.insert(path.to_owned(), current);
        }
    }
}

/// How strictly two lines must agree.
#[derive(Debug, Clone, Copy)]
enum Fuzz {
    Exact,
    TrimEnd,
    Trim,
}

impl Fuzz {
    fn same(self, a: &str, b: &str) -> bool {
        match self {
            Self::Exact => a == b,
            Self::TrimEnd => a.trim_end() == b.trim_end(),
            Self::Trim => a.trim() == b.trim(),
        }
    }
}

/// Applies the hunks of one file in order.
fn apply_hunks(path: &str, text: &str, hunks: &[Hunk]) -> Result<String, PlanError> {
    // Keep the file's line endings: CRLF files stay CRLF.
    let crlf = text.contains("\r\n");
    let eol = if crlf { "\r\n" } else { "\n" };
    let trailing_newline = text.ends_with('\n');
    let body = text.strip_suffix('\n').unwrap_or(text);
    let mut lines: Vec<String> = if text.is_empty() {
        Vec::new()
    } else {
        body.split('\n')
            .map(|l| {
                if crlf {
                    l.strip_suffix('\r').unwrap_or(l)
                } else {
                    l
                }
                .to_owned()
            })
            .collect()
    };
    let mut cursor = 0;
    for (n, hunk) in hunks.iter().enumerate() {
        let n = n + 1;
        let old: Vec<&str> = hunk
            .lines
            .iter()
            .filter_map(|l| match l {
                HunkLine::Context(t) | HunkLine::Remove(t) => Some(t.as_str()),
                HunkLine::Add(_) => None,
            })
            .collect();
        let mut from = cursor;
        if let Some(anchor) = &hunk.anchor {
            let found = (cursor..lines.len())
                .find(|&i| lines[i].trim() == anchor.trim())
                .or_else(|| (cursor..lines.len()).find(|&i| lines[i].contains(anchor.trim())));
            match found {
                // Context may start on the anchor line itself.
                Some(i) => from = i,
                None => {
                    return Err(PlanError(format!(
                        "{path}, hunk {n}: anchor {anchor:?} not found"
                    )));
                }
            }
        }
        let at = if old.is_empty() {
            // Pure insertion: after the anchor line, or at the end of the file.
            if hunk.anchor.is_some() && !hunk.at_eof {
                from + 1
            } else {
                lines.len()
            }
        } else {
            locate(&lines, &old, from, hunk.at_eof).map_err(|problem| {
                PlanError(format!(
                    "{path}, hunk {n}: {problem}. Expected these lines:\n{}",
                    old.iter()
                        .take(6)
                        .map(|l| format!("  {l}"))
                        .collect::<Vec<_>>()
                        .join("\n")
                ))
            })?
        };
        // Context lines keep the file's own text (it may differ in whitespace).
        let mut replacement = Vec::with_capacity(hunk.lines.len());
        let mut k = at;
        for line in &hunk.lines {
            match line {
                HunkLine::Context(_) => {
                    replacement.push(lines.get(k).cloned().unwrap_or_default());
                    k += 1;
                }
                HunkLine::Remove(_) => k += 1,
                HunkLine::Add(t) => replacement.push(t.clone()),
            }
        }
        let end = (at + old.len()).min(lines.len());
        let added = replacement.len();
        lines.splice(at..end, replacement);
        cursor = at + added;
    }
    let mut out = lines.join(eol);
    if !lines.is_empty() && (trailing_newline || text.is_empty()) {
        out.push_str(eol);
    }
    Ok(out)
}

/// Finds the unique position of `old` in `lines` at or after `from`.
fn locate(lines: &[String], old: &[&str], from: usize, at_eof: bool) -> Result<usize, String> {
    if old.len() > lines.len() {
        return Err("the context is longer than the file".into());
    }
    let last_start = lines.len() - old.len();
    for fuzz in [Fuzz::Exact, Fuzz::TrimEnd, Fuzz::Trim] {
        let matches = |i: usize| {
            old.iter()
                .enumerate()
                .all(|(j, o)| fuzz.same(&lines[i + j], o))
        };
        let found: Vec<usize> = if at_eof {
            (last_start >= from && matches(last_start))
                .then_some(last_start)
                .into_iter()
                .collect()
        } else {
            (from..=last_start)
                .filter(|&i| matches(i))
                .take(2)
                .collect()
        };
        match found.as_slice() {
            [one] => return Ok(*one),
            [] => {}
            _ => {
                return Err(
                    "the context matches more than one place; add more context lines or an @@ anchor"
                        .into(),
                );
            }
        }
    }
    Err(if at_eof {
        "the context was not found at the end of the file".into()
    } else {
        "the context was not found".into()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::patch::parse_patch;

    fn fs(files: &[(&str, &str)]) -> HashMap<String, String> {
        files
            .iter()
            .map(|(p, c)| ((*p).to_owned(), (*c).to_owned()))
            .collect()
    }

    fn run(files: &[(&str, &str)], body: &str) -> Result<Vec<FileChange>, PlanError> {
        let patch = parse_patch(&format!("*** Begin Patch\n{body}\n*** End Patch")).unwrap();
        plan(&patch, &fs(files))
    }

    fn after(changes: &[FileChange], path: &str) -> Option<String> {
        changes
            .iter()
            .find(|c| c.path == path)
            .unwrap_or_else(|| panic!("no change for {path}: {changes:?}"))
            .after
            .clone()
    }

    #[test]
    fn add_creates_a_file_with_a_trailing_newline() {
        let c = run(&[], "*** Add File: a/b.txt\n+one\n+two").unwrap();
        assert_eq!(c[0].before, None);
        assert_eq!(after(&c, "a/b.txt").as_deref(), Some("one\ntwo\n"));
    }

    #[test]
    fn add_refuses_an_existing_file() {
        let e = run(&[("a", "x")], "*** Add File: a\n+y").unwrap_err();
        assert!(e.0.contains("already exists"), "{e}");
    }

    #[test]
    fn delete_removes_and_requires_the_file() {
        let c = run(&[("a", "x")], "*** Delete File: a").unwrap();
        assert_eq!(
            (c[0].before.as_deref(), c[0].after.as_deref()),
            (Some("x"), None)
        );
        assert!(
            run(&[], "*** Delete File: a")
                .unwrap_err()
                .0
                .contains("no such file")
        );
    }

    #[test]
    fn update_replaces_lines_and_keeps_the_rest() {
        let c = run(
            &[("f", "a\nb\nc\nd\n")],
            "*** Update File: f\n@@\n b\n-c\n+C\n+C2",
        )
        .unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("a\nb\nC\nC2\nd\n"));
    }

    #[test]
    fn update_keeps_a_missing_trailing_newline() {
        let c = run(&[("f", "a\nb")], "*** Update File: f\n@@\n-b\n+B").unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("a\nB"));
    }

    #[test]
    fn crlf_files_keep_crlf_line_endings() {
        let c = run(&[("f", "a\r\nb\r\n")], "*** Update File: f\n@@\n a\n-b\n+B").unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("a\r\nB\r\n"));
    }

    #[test]
    fn hunks_apply_in_order_after_each_other() {
        let c = run(
            &[("f", "x\n1\nx\n2\n")],
            "*** Update File: f\n@@\n-x\n+first\n 1\n@@\n-x\n+second",
        )
        .unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("first\n1\nsecond\n2\n"));
    }

    #[test]
    fn ambiguous_context_is_refused() {
        let e = run(
            &[("f", "x\ny\nx\ny\n")],
            "*** Update File: f\n@@\n x\n-y\n+z",
        )
        .unwrap_err();
        assert!(e.0.contains("more than one place"), "{e}");
    }

    #[test]
    fn an_anchor_disambiguates() {
        let c = run(
            &[("f", "fn a() {\n  x\n}\nfn b() {\n  x\n}\n")],
            "*** Update File: f\n@@ fn b() {\n-  x\n+  y",
        )
        .unwrap();
        assert_eq!(
            after(&c, "f").as_deref(),
            Some("fn a() {\n  x\n}\nfn b() {\n  y\n}\n")
        );
    }

    #[test]
    fn whitespace_differences_fall_back_and_keep_the_files_context() {
        let c = run(
            &[("f", "    keep  \n    old\n")],
            "*** Update File: f\n@@\n keep\n-old\n+    new",
        )
        .unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("    keep  \n    new\n"));
    }

    #[test]
    fn missing_context_names_the_file_hunk_and_lines() {
        let e = run(&[("f", "a\n")], "*** Update File: f\n@@\n-zzz\n+y").unwrap_err();
        assert!(
            e.0.starts_with("f, hunk 1: the context was not found"),
            "{e}"
        );
        assert!(e.0.contains("  zzz"), "{e}");
    }

    #[test]
    fn end_of_file_hunks_match_only_at_the_end() {
        let files = [("f", "x\nmid\nx\n")];
        let c = run(&files, "*** Update File: f\n@@\n-x\n+END\n*** End of File").unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("x\nmid\nEND\n"));
    }

    #[test]
    fn pure_insertions_go_after_the_anchor_or_at_the_end() {
        let c = run(&[("f", "a\nb\n")], "*** Update File: f\n@@ a\n+inserted").unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("a\ninserted\nb\n"));
        let c = run(&[("f", "a\nb\n")], "*** Update File: f\n@@\n+tail").unwrap();
        assert_eq!(after(&c, "f").as_deref(), Some("a\nb\ntail\n"));
    }

    #[test]
    fn move_renames_and_refuses_to_overwrite() {
        let c = run(
            &[("a", "1\n")],
            "*** Update File: a\n*** Move to: b\n@@\n-1\n+2",
        )
        .unwrap();
        assert_eq!(after(&c, "a"), None);
        assert_eq!(after(&c, "b").as_deref(), Some("2\n"));
        let e = run(
            &[("a", "1\n"), ("b", "x")],
            "*** Update File: a\n*** Move to: b\n@@\n-1\n+2",
        )
        .unwrap_err();
        assert!(e.0.contains("already exists"), "{e}");
    }

    #[test]
    fn later_ops_see_earlier_ones_and_paths_are_normalised() {
        let c = run(&[], "*** Add File: ./a\n+1\n*** Update File: a\n@@\n-1\n+2").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(after(&c, "a").as_deref(), Some("2\n"));
    }

    #[test]
    fn a_failing_op_fails_the_whole_plan() {
        let e = run(
            &[("a", "1\n")],
            "*** Update File: a\n@@\n-1\n+2\n*** Delete File: missing",
        );
        assert!(e.is_err());
    }

    #[test]
    fn changes_that_cancel_out_are_dropped() {
        let c = run(&[("a", "1\n")], "*** Delete File: a\n*** Add File: a\n+1").unwrap();
        assert!(c.is_empty(), "{c:?}");
    }

    #[test]
    fn unreadable_files_are_reported() {
        struct Broken;
        impl PatchFs for Broken {
            fn read(&self, _: &str) -> Result<Option<String>, String> {
                Err("not UTF-8".into())
            }
        }
        let patch = parse_patch("*** Begin Patch\n*** Delete File: a\n*** End Patch").unwrap();
        assert!(plan(&patch, &Broken).unwrap_err().0.contains("not UTF-8"));
    }
}
