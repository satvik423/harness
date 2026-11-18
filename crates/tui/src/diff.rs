//! Unified diffs for approval previews, tool cards and `/diff` (`similar`).

use similar::{ChangeTag, TextDiff};

/// One line of a rendered diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffLine {
    /// `@@ … @@` hunk header.
    Header(String),
    /// Unchanged context.
    Same(String),
    /// Removed.
    Removed(String),
    /// Added.
    Added(String),
    /// A note such as "… 40 more lines".
    Note(String),
}

/// A unified diff of `before` → `after` with 3 lines of context, at most
/// `max_lines` lines (a note says how many were left out).
///
/// # Examples
/// ```
/// use harness_tui::diff::{unified, DiffLine};
/// let d = unified("a\nb\n", "a\nc\n", 50);
/// assert!(d.contains(&DiffLine::Removed("b".into())));
/// assert!(d.contains(&DiffLine::Added("c".into())));
/// ```
#[must_use]
pub fn unified(before: &str, after: &str, max_lines: usize) -> Vec<DiffLine> {
    let diff = TextDiff::from_lines(before, after);
    let mut out = Vec::new();
    for group in diff.grouped_ops(3) {
        let (Some(first), Some(last)) = (group.first(), group.last()) else {
            continue;
        };
        let old = first.old_range().start..last.old_range().end;
        let new = first.new_range().start..last.new_range().end;
        out.push(DiffLine::Header(format!(
            "@@ -{},{} +{},{} @@",
            old.start + 1,
            old.len(),
            new.start + 1,
            new.len()
        )));
        for op in &group {
            for change in diff.iter_changes(op) {
                let text = change.value().trim_end_matches(['\n', '\r']).to_owned();
                out.push(match change.tag() {
                    ChangeTag::Equal => DiffLine::Same(text),
                    ChangeTag::Delete => DiffLine::Removed(text),
                    ChangeTag::Insert => DiffLine::Added(text),
                });
            }
        }
    }
    if out.len() > max_lines {
        let rest = out.len() - max_lines;
        out.truncate(max_lines);
        out.push(DiffLine::Note(format!("… {rest} more lines")));
    }
    out
}

/// Added and removed line counts.
#[must_use]
pub fn counts(lines: &[DiffLine]) -> (usize, usize) {
    lines.iter().fold((0, 0), |(a, r), l| match l {
        DiffLine::Added(_) => (a + 1, r),
        DiffLine::Removed(_) => (a, r + 1),
        _ => (a, r),
    })
}

/// Colours an `apply_patch` text like a diff (`+`, `-`, `@@`, `***` headers).
#[must_use]
pub fn patch_lines(patch: &str, max_lines: usize) -> Vec<DiffLine> {
    let mut out: Vec<DiffLine> = patch
        .lines()
        .map(|l| {
            if l.starts_with("***") || l.starts_with("@@") {
                DiffLine::Header(l.to_owned())
            } else if let Some(rest) = l.strip_prefix('+') {
                DiffLine::Added(rest.to_owned())
            } else if let Some(rest) = l.strip_prefix('-') {
                DiffLine::Removed(rest.to_owned())
            } else {
                DiffLine::Same(l.strip_prefix(' ').unwrap_or(l).to_owned())
            }
        })
        .collect();
    if out.len() > max_lines {
        let rest = out.len() - max_lines;
        out.truncate(max_lines);
        out.push(DiffLine::Note(format!("… {rest} more lines")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_change_has_a_header_context_and_both_sides() {
        let before = (1..=10).map(|i| format!("{i}\n")).collect::<String>();
        let after = before.replace("5\n", "five\n");
        let d = unified(&before, &after, 100);
        assert_eq!(d[0], DiffLine::Header("@@ -2,7 +2,7 @@".into()));
        assert_eq!(counts(&d), (1, 1));
        assert!(d.contains(&DiffLine::Same("4".into())));
    }

    #[test]
    fn new_files_are_all_additions_and_long_diffs_are_cut() {
        let after = (1..=30).map(|i| format!("{i}\n")).collect::<String>();
        let d = unified("", &after, 10);
        assert_eq!(d.len(), 11);
        assert_eq!(d[10], DiffLine::Note("… 21 more lines".into()));
        assert!(unified("same\n", "same\n", 10).is_empty());
    }

    #[test]
    fn patches_are_coloured_by_prefix() {
        let d = patch_lines("*** Update File: a\n@@\n x\n-y\n+z", 50);
        assert_eq!(
            d,
            [
                DiffLine::Header("*** Update File: a".into()),
                DiffLine::Header("@@".into()),
                DiffLine::Same("x".into()),
                DiffLine::Removed("y".into()),
                DiffLine::Added("z".into()),
            ]
        );
    }
}
