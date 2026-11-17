//! [`ToolRegistry`]: the set of tools available to a task, keyed by name.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use thiserror::Error;

use crate::{Tool, ToolName, ToolSpec};

/// Error returned when two tools share a name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("a tool named {0} is already registered")]
pub struct DuplicateTool(pub ToolName);

/// Tools available to a task. Iteration and [`ToolRegistry::specs`] are sorted
/// by name, so the tool list sent to the model is stable (it is frozen per
/// segment and part of the cached prefix).
///
/// # Examples
/// ```
/// use harness_ports::ToolRegistry;
/// let registry = ToolRegistry::new();
/// assert!(registry.is_empty());
/// assert!(registry.specs().is_empty());
/// ```
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<ToolName, Arc<dyn Tool>>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a tool.
    ///
    /// # Errors
    /// [`DuplicateTool`] if a tool with the same name is already registered.
    pub fn register(&mut self, tool: Arc<dyn Tool>) -> Result<(), DuplicateTool> {
        let name = tool.spec().name;
        if self.tools.contains_key(&name) {
            return Err(DuplicateTool(name));
        }
        self.tools.insert(name, tool);
        Ok(())
    }

    /// Looks a tool up by name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<Arc<dyn Tool>> {
        self.tools.get(name).map(Arc::clone)
    }

    /// Specs of every tool, sorted by name.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.values().map(|t| t.spec()).collect()
    }

    /// Keeps only the tools for which `keep` returns `true` (persona filters).
    pub fn retain(&mut self, mut keep: impl FnMut(&ToolName) -> bool) {
        self.tools.retain(|name, _| keep(name));
    }

    /// Number of tools.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// `true` if there are no tools.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{RiskCtx, ToolCtx, ToolError, ToolOutput};
    use async_trait::async_trait;
    use harness_types::Risk;
    use serde_json::{Value, json};

    struct Named(&'static str);

    #[async_trait]
    impl Tool for Named {
        fn spec(&self) -> ToolSpec {
            ToolSpec {
                name: ToolName::new(self.0).unwrap(),
                description: format!("tool {}", self.0),
                input_schema: json!({"type": "object"}),
                strict: false,
                eager_input_streaming: false,
            }
        }
        fn risk(&self, _: &Value, _: &RiskCtx) -> Risk {
            Risk::ReadOnly { paths: vec![] }
        }
        async fn run(&self, _: Value, _: ToolCtx) -> Result<ToolOutput, ToolError> {
            Ok(ToolOutput::default())
        }
    }

    fn registry(names: &[&'static str]) -> ToolRegistry {
        let mut r = ToolRegistry::new();
        for n in names {
            r.register(Arc::new(Named(n))).unwrap();
        }
        r
    }

    #[test]
    fn specs_are_sorted_by_name() {
        let r = registry(&["shell", "glob", "read_file"]);
        let names: Vec<String> = r.specs().into_iter().map(|s| s.name.to_string()).collect();
        assert_eq!(names, ["glob", "read_file", "shell"]);
    }

    #[test]
    fn duplicate_names_are_rejected() {
        let mut r = registry(&["shell"]);
        let err = r.register(Arc::new(Named("shell"))).unwrap_err();
        assert_eq!(err.0.as_str(), "shell");
        assert_eq!(r.len(), 1);
    }

    #[test]
    fn get_finds_tools_by_name() {
        let r = registry(&["shell"]);
        assert!(r.get("shell").is_some());
        assert!(r.get("nope").is_none());
    }

    #[test]
    fn retain_filters_tools() {
        let mut r = registry(&["shell", "read_file"]);
        r.retain(|n| n.as_str() == "read_file");
        assert_eq!(r.len(), 1);
        assert!(r.get("shell").is_none());
    }
}
