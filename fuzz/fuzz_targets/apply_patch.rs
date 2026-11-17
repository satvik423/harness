//! Fuzzes the `apply_patch` parser and planner (P2-T4).
//!
//! Input: patch text, then the contents of files `a` and `b`, separated by NUL
//! bytes. The parser and planner must never panic, and a successful plan may
//! only touch paths the patch names.
#![no_main]

use std::collections::HashMap;

use harness_tools::patch::{parse_patch, plan};
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let text = String::from_utf8_lossy(data);
    let mut parts = text.split('\0');
    let patch_text = parts.next().unwrap_or_default();
    let mut files = HashMap::new();
    for name in ["a", "b"] {
        if let Some(contents) = parts.next() {
            files.insert(name.to_string(), contents.to_string());
        }
    }
    let Ok(patch) = parse_patch(patch_text) else {
        return;
    };
    // Rendering and re-parsing must agree with the first parse.
    assert_eq!(parse_patch(&patch.render()).as_ref(), Ok(&patch));
    if let Ok(changes) = plan(&patch, &files) {
        let named = patch.paths();
        for change in changes {
            assert!(named.iter().any(|p| harness_tools::patch::normalize(p) == change.path));
        }
    }
});
