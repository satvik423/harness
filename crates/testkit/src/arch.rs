//! Layering checks (P0-T9): the dependency directions from SDD 2.4, enforced in tests.
//!
//! [`DependencyGraph::from_workspace`] reads `cargo metadata`, keeps only
//! normal (non-dev, non-build) dependencies between workspace crates, and
//! [`check`] reports every edge a [`LayerRule`] forbids. Every workspace crate
//! must have a rule, so adding a crate forces a decision about its layer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use thiserror::Error;

/// Errors from building the dependency graph.
#[derive(Debug, Error)]
pub enum ArchError {
    /// `cargo metadata` could not run or failed.
    #[error("cargo metadata failed: {0}")]
    Cargo(String),
    /// Its output was not what we expected.
    #[error("unexpected cargo metadata output: {0}")]
    Metadata(String),
}

/// Normal dependencies between workspace crates, by package name.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DependencyGraph {
    edges: BTreeMap<String, BTreeSet<String>>,
}

impl DependencyGraph {
    /// Builds a graph from explicit `(crate, dependency)` pairs (for tests).
    /// Crates listed only as dependents still appear as nodes.
    #[must_use]
    pub fn from_edges<'a>(
        crates: &[&str],
        edges: impl IntoIterator<Item = (&'a str, &'a str)>,
    ) -> Self {
        let mut graph = Self::default();
        for name in crates {
            graph.edges.entry((*name).to_owned()).or_default();
        }
        for (from, to) in edges {
            graph.edges.entry(to.to_owned()).or_default();
            graph
                .edges
                .entry(from.to_owned())
                .or_default()
                .insert(to.to_owned());
        }
        graph
    }

    /// Builds the graph from `cargo metadata --format-version 1 --no-deps` JSON.
    ///
    /// # Errors
    /// [`ArchError::Metadata`] if required fields are missing.
    pub fn from_metadata(metadata: &Value) -> Result<Self, ArchError> {
        let packages = metadata["packages"]
            .as_array()
            .ok_or_else(|| ArchError::Metadata("missing \"packages\" array".into()))?;
        let name_of = |p: &Value| -> Result<String, ArchError> {
            p["name"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ArchError::Metadata("package without a name".into()))
        };
        let names: BTreeSet<String> = packages.iter().map(name_of).collect::<Result<_, _>>()?;
        let mut graph = Self::default();
        for package in packages {
            let name = name_of(package)?;
            let deps = graph.edges.entry(name).or_default();
            for dep in package["dependencies"].as_array().into_iter().flatten() {
                // `kind` is null for normal dependencies, "dev" or "build" otherwise.
                let Some(dep_name) = dep["name"].as_str() else {
                    continue;
                };
                if dep["kind"].is_null() && names.contains(dep_name) {
                    deps.insert(dep_name.to_owned());
                }
            }
        }
        Ok(graph)
    }

    /// Runs `cargo metadata` in `workspace_dir` and builds the graph.
    ///
    /// # Errors
    /// [`ArchError::Cargo`] if cargo fails, [`ArchError::Metadata`] for bad output.
    pub fn from_workspace(workspace_dir: &Path) -> Result<Self, ArchError> {
        let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
        let output = Command::new(cargo)
            .args([
                "metadata",
                "--format-version",
                "1",
                "--no-deps",
                "--offline",
            ])
            .current_dir(workspace_dir)
            .output()
            .map_err(|e| ArchError::Cargo(e.to_string()))?;
        if !output.status.success() {
            return Err(ArchError::Cargo(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        let json: Value = serde_json::from_slice(&output.stdout)
            .map_err(|e| ArchError::Metadata(e.to_string()))?;
        Self::from_metadata(&json)
    }

    /// Workspace crate names.
    pub fn crates(&self) -> impl Iterator<Item = &str> {
        self.edges.keys().map(String::as_str)
    }

    /// Direct workspace dependencies of `krate`.
    pub fn deps_of(&self, krate: &str) -> impl Iterator<Item = &str> {
        self.edges
            .get(krate)
            .into_iter()
            .flatten()
            .map(String::as_str)
    }
}

/// What a crate may depend on inside the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Allowed {
    /// Only these workspace crates (an allowlist; anything else is a violation).
    Only(&'static [&'static str]),
    /// Any workspace crate except these.
    AllExcept(&'static [&'static str]),
}

/// The layering rule for one crate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerRule {
    /// Package name.
    pub krate: &'static str,
    /// What it may depend on.
    pub allowed: Allowed,
}

/// One forbidden dependency edge, or a crate without a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// The crate breaking the rule.
    pub krate: String,
    /// The forbidden dependency, or `None` if the crate has no rule at all.
    pub depends_on: Option<String>,
}

impl std::fmt::Display for Violation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match &self.depends_on {
            Some(dep) => write!(f, "{} must not depend on {dep}", self.krate),
            None => write!(
                f,
                "{} has no layering rule; add one to harness_rules()",
                self.krate
            ),
        }
    }
}

/// Crates no adapter may depend on: the runtime layer, front-ends and test support.
pub const ADAPTER_FORBIDDEN: &[&str] = &[
    "harness-runtime",
    "harness-context",
    "harness-cli",
    "harness-tui",
    "harness-server",
    "harness-testkit",
];

/// The rule for an adapter crate (`provider-*`, `store-*`, `tools`, `mcp`, …):
/// anything except [`ADAPTER_FORBIDDEN`].
#[must_use]
pub const fn adapter(krate: &'static str) -> LayerRule {
    LayerRule {
        krate,
        allowed: Allowed::AllExcept(ADAPTER_FORBIDDEN),
    }
}

/// The project's layering rules (SDD 2.4, CLAUDE.md rule 1).
///
/// Adapters get a rule (via [`adapter`]) in the phase that creates them; until
/// then the "every crate needs a rule" check catches any new crate.
#[must_use]
pub fn harness_rules() -> Vec<LayerRule> {
    const TYPES: &str = "harness-types";
    const PROTOCOL: &str = "harness-protocol";
    const PORTS: &str = "harness-ports";
    vec![
        LayerRule {
            krate: TYPES,
            allowed: Allowed::Only(&[]),
        },
        LayerRule {
            krate: PROTOCOL,
            allowed: Allowed::Only(&[TYPES]),
        },
        LayerRule {
            krate: PORTS,
            allowed: Allowed::Only(&[TYPES, PROTOCOL]),
        },
        LayerRule {
            krate: "harness-config",
            allowed: Allowed::Only(&[TYPES]),
        },
        LayerRule {
            krate: "harness-context",
            allowed: Allowed::Only(&[TYPES, PROTOCOL, PORTS]),
        },
        LayerRule {
            krate: "harness-runtime",
            allowed: Allowed::Only(&[TYPES, PROTOCOL, PORTS, "harness-context"]),
        },
        LayerRule {
            krate: "harness-testkit",
            allowed: Allowed::Only(&[TYPES, PROTOCOL, PORTS]),
        },
        adapter("harness-tools"),
        adapter("harness-provider-anthropic"),
        adapter("harness-provider-openai"),
        adapter("harness-policy"),
        adapter("harness-store-sqlite"),
        // A front-end: it renders events and sends ops; adapters are wired in cli.
        LayerRule {
            krate: "harness-tui",
            allowed: Allowed::Only(&[TYPES, PROTOCOL, PORTS, "harness-config"]),
        },
        LayerRule {
            krate: "harness-cli",
            allowed: Allowed::AllExcept(&["harness-testkit"]),
        },
        // An offline/live eval runner (Phase 5): wires like cli and checks the
        // log with the testkit's invariant checks; never a front-end.
        LayerRule {
            krate: "harness-evals",
            allowed: Allowed::AllExcept(&["harness-tui"]),
        },
    ]
}

/// Returns every violation of `rules` in `graph`, sorted.
#[must_use]
pub fn check(graph: &DependencyGraph, rules: &[LayerRule]) -> Vec<Violation> {
    let by_name: BTreeMap<&str, &LayerRule> = rules.iter().map(|r| (r.krate, r)).collect();
    let mut violations = Vec::new();
    for krate in graph.crates() {
        let Some(rule) = by_name.get(krate) else {
            violations.push(Violation {
                krate: krate.to_owned(),
                depends_on: None,
            });
            continue;
        };
        for dep in graph.deps_of(krate) {
            let ok = match rule.allowed {
                Allowed::Only(list) => list.contains(&dep),
                Allowed::AllExcept(list) => !list.contains(&dep),
            };
            if !ok {
                violations.push(Violation {
                    krate: krate.to_owned(),
                    depends_on: Some(dep.to_owned()),
                });
            }
        }
    }
    violations.sort_by(|a, b| (&a.krate, &a.depends_on).cmp(&(&b.krate, &b.depends_on)));
    violations
}

#[cfg(test)]
mod tests {
    use super::*;

    const CORE: &[&str] = &["harness-types", "harness-protocol", "harness-ports"];

    fn rules() -> Vec<LayerRule> {
        vec![
            LayerRule {
                krate: "harness-types",
                allowed: Allowed::Only(&[]),
            },
            LayerRule {
                krate: "harness-protocol",
                allowed: Allowed::Only(&["harness-types"]),
            },
            LayerRule {
                krate: "harness-ports",
                allowed: Allowed::Only(&["harness-types", "harness-protocol"]),
            },
            LayerRule {
                krate: "harness-runtime",
                allowed: Allowed::Only(&["harness-types", "harness-protocol", "harness-ports"]),
            },
            LayerRule {
                krate: "harness-provider-anthropic",
                allowed: Allowed::AllExcept(&["harness-runtime"]),
            },
        ]
    }

    #[test]
    fn runtime_depending_on_an_adapter_is_a_violation() {
        let crates = [CORE, &["harness-runtime", "harness-provider-anthropic"]].concat();
        let g = DependencyGraph::from_edges(
            &crates,
            [
                ("harness-runtime", "harness-provider-anthropic"),
                ("harness-runtime", "harness-ports"),
            ],
        );
        assert_eq!(
            check(&g, &rules()),
            vec![Violation {
                krate: "harness-runtime".into(),
                depends_on: Some("harness-provider-anthropic".into())
            }]
        );
    }

    #[test]
    fn adapter_depending_on_runtime_is_a_violation() {
        let crates = [CORE, &["harness-runtime", "harness-provider-anthropic"]].concat();
        let g = DependencyGraph::from_edges(
            &crates,
            [("harness-provider-anthropic", "harness-runtime")],
        );
        assert_eq!(check(&g, &rules()).len(), 1);
    }

    #[test]
    fn allowed_edges_pass() {
        let g = DependencyGraph::from_edges(
            CORE,
            [
                ("harness-protocol", "harness-types"),
                ("harness-ports", "harness-protocol"),
            ],
        );
        assert!(check(&g, &rules()).is_empty());
    }

    #[test]
    fn crate_without_rule_is_reported() {
        let g = DependencyGraph::from_edges(&["harness-types", "harness-new"], []);
        assert_eq!(
            check(&g, &rules()),
            vec![Violation {
                krate: "harness-new".into(),
                depends_on: None
            }]
        );
    }

    #[test]
    fn metadata_keeps_only_normal_workspace_dependencies() {
        let json = serde_json::json!({
            "packages": [
                {"name": "harness-ports", "dependencies": [
                    {"name": "harness-types", "kind": null},
                    {"name": "harness-testkit", "kind": "dev"},
                    {"name": "serde", "kind": null}
                ]},
                {"name": "harness-types", "dependencies": []},
                {"name": "harness-testkit", "dependencies": [{"name": "harness-ports", "kind": null}]}
            ]
        });
        let g = DependencyGraph::from_metadata(&json).unwrap();
        assert_eq!(
            g.deps_of("harness-ports").collect::<Vec<_>>(),
            ["harness-types"]
        );
        assert_eq!(g.crates().count(), 3);
    }

    #[test]
    fn adapter_rule_forbids_runtime_and_testkit() {
        let rule = adapter("harness-tools");
        let g = DependencyGraph::from_edges(
            &[
                "harness-tools",
                "harness-ports",
                "harness-runtime",
                "harness-testkit",
            ],
            [
                ("harness-tools", "harness-ports"),
                ("harness-tools", "harness-runtime"),
                ("harness-tools", "harness-testkit"),
            ],
        );
        let others = [
            LayerRule {
                krate: "harness-ports",
                allowed: Allowed::Only(&[]),
            },
            LayerRule {
                krate: "harness-runtime",
                allowed: Allowed::Only(&[]),
            },
            LayerRule {
                krate: "harness-testkit",
                allowed: Allowed::Only(&[]),
            },
        ];
        let rules: Vec<LayerRule> = std::iter::once(rule).chain(others).collect();
        let deps: Vec<_> = check(&g, &rules)
            .into_iter()
            .filter_map(|v| v.depends_on)
            .collect();
        assert_eq!(deps, ["harness-runtime", "harness-testkit"]);
    }

    #[test]
    fn metadata_without_packages_is_an_error() {
        assert!(DependencyGraph::from_metadata(&serde_json::json!({})).is_err());
    }

    #[test]
    fn project_rules_cover_core_crates_and_forbid_adapter_imports_in_runtime() {
        let r = harness_rules();
        for name in [
            "harness-types",
            "harness-protocol",
            "harness-ports",
            "harness-config",
            "harness-runtime",
            "harness-context",
            "harness-testkit",
            "harness-cli",
        ] {
            assert!(r.iter().any(|x| x.krate == name), "missing rule for {name}");
        }
        let g = DependencyGraph::from_edges(
            &[
                "harness-types",
                "harness-protocol",
                "harness-ports",
                "harness-runtime",
                "harness-store-sqlite",
            ],
            [("harness-runtime", "harness-store-sqlite")],
        );
        let v = check(&g, &r);
        assert!(v.iter().any(|x| x.krate == "harness-runtime"), "{v:?}");
    }
}
