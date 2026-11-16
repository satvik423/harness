//! Enforces the workspace's layering rules against the real `cargo metadata`.

use std::path::Path;

use harness_testkit::arch::{DependencyGraph, check, harness_rules};

#[test]
fn workspace_respects_layering_rules() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let graph = DependencyGraph::from_workspace(&workspace).expect("cargo metadata");
    let violations = check(&graph, &harness_rules());
    assert!(
        violations.is_empty(),
        "layering violations:\n{}",
        violations
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
}
