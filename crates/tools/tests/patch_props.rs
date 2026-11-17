//! Property tests for the `apply_patch` parser and planner (P2-T4).

#![allow(clippy::unwrap_used)]

use std::collections::HashMap;

use harness_tools::patch::{Hunk, HunkLine, Patch, PatchOp, parse_patch, plan};
use proptest::prelude::*;

fn path() -> impl Strategy<Value = String> {
    "[a-z]{1,8}(/[a-z]{1,8}){0,2}(\\.[a-z]{1,3})?"
}

/// Any line text a file can hold (no line breaks).
fn line_text() -> impl Strategy<Value = String> {
    "[^\n]{0,30}"
}

fn anchor() -> impl Strategy<Value = Option<String>> {
    proptest::option::of("[a-z(){}][a-z(){} ]{0,10}[a-z(){}]")
}

fn hunk() -> impl Strategy<Value = Hunk> {
    let line = prop_oneof![
        line_text().prop_map(HunkLine::Context),
        line_text().prop_map(HunkLine::Remove),
        line_text().prop_map(HunkLine::Add),
    ];
    (
        anchor(),
        proptest::collection::vec(line, 0..6),
        line_text(),
        any::<bool>(),
    )
        .prop_map(|(anchor, mut lines, change, at_eof)| {
            // Every hunk changes something, and does not end in empty context
            // (the parser reads trailing blank lines as separators).
            lines.push(HunkLine::Add(change));
            Hunk {
                anchor,
                lines,
                at_eof,
            }
        })
}

fn op() -> impl Strategy<Value = PatchOp> {
    prop_oneof![
        (path(), proptest::collection::vec(line_text(), 0..5))
            .prop_map(|(path, lines)| PatchOp::Add { path, lines }),
        path().prop_map(|path| PatchOp::Delete { path }),
        (
            path(),
            proptest::option::of(path()),
            proptest::collection::vec(hunk(), 1..4)
        )
            .prop_map(|(path, move_to, hunks)| PatchOp::Update {
                path,
                move_to,
                hunks
            }),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(512))]

    /// Rendering a patch and parsing it back gives the same patch.
    #[test]
    fn render_then_parse_round_trips(ops in proptest::collection::vec(op(), 1..5)) {
        let patch = Patch { ops };
        let text = patch.render();
        prop_assert_eq!(parse_patch(&text).unwrap(), patch);
    }

    /// The parser never panics, whatever the input.
    #[test]
    fn parser_never_panics(text in "(\\*\\*\\* (Begin Patch|End Patch|Add File: a|Update File: a|Delete File: a|Move to: b|End of File)|@@[ a-z]{0,4}|[ +-][a-z]{0,4}|[^\n]{0,8})(\n(\\*\\*\\* (Begin Patch|End Patch|Add File: a|Update File: a|Delete File: a|Move to: b|End of File)|@@[ a-z]{0,4}|[ +-][a-z]{0,4}|[^\n]{0,8})){0,12}") {
        let _ = parse_patch(&text);
    }

    /// Planning any parsed patch against any small file set never panics, and
    /// a successful plan only names paths from the patch.
    #[test]
    fn planner_never_panics(
        ops in proptest::collection::vec(op(), 1..4),
        files in proptest::collection::hash_map(path(), "([a-z ]{0,6}\n){0,6}", 0..4),
    ) {
        let patch = Patch { ops };
        let fs: HashMap<String, String> = files;
        if let Ok(changes) = plan(&patch, &fs) {
            let named = patch.paths();
            for c in changes {
                prop_assert!(named.contains(&c.path.as_str()));
            }
        }
    }

    /// Replacing any range of a file with distinct lines, with up to three
    /// context lines on each side, gives exactly the expected file.
    #[test]
    fn replacing_a_range_gives_the_expected_file(
        len in 1usize..40,
        start_frac in 0.0f64..1.0,
        span in 0usize..5,
        context in 0usize..4,
        new_lines in proptest::collection::vec("[a-z]{1,6}", 0..4),
    ) {
        let lines: Vec<String> = (0..len).map(|i| format!("line {i}")).collect();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, clippy::cast_precision_loss)]
        let start = ((len as f64) * start_frac) as usize;
        let end = (start + span).min(len);
        prop_assume!(end > start || !new_lines.is_empty());
        let before = start.saturating_sub(context);
        let after = (end + context).min(len);

        let mut hunk = Vec::new();
        hunk.extend(lines[before..start].iter().cloned().map(HunkLine::Context));
        hunk.extend(lines[start..end].iter().cloned().map(HunkLine::Remove));
        hunk.extend(new_lines.iter().map(|l| HunkLine::Add(format!("new {l}"))));
        hunk.extend(lines[end..after].iter().cloned().map(HunkLine::Context));
        // A hunk with no context and no removals can only append.
        prop_assume!(before < after || start == len);

        let patch = Patch {
            ops: vec![PatchOp::Update {
                path: "f".into(),
                move_to: None,
                hunks: vec![Hunk { anchor: None, lines: hunk, at_eof: false }],
            }],
        };
        let fs = HashMap::from([("f".to_string(), lines.join("\n") + "\n")]);
        let changes = plan(&patch, &fs).unwrap();

        let mut expected = lines[..start].to_vec();
        expected.extend(new_lines.iter().map(|l| format!("new {l}")));
        expected.extend(lines[end..].iter().cloned());
        let expected = if expected.is_empty() { String::new() } else { expected.join("\n") + "\n" };
        let got = changes.first().and_then(|c| c.after.clone()).unwrap_or_else(|| fs["f"].clone());
        prop_assert_eq!(got, expected);
    }
}
