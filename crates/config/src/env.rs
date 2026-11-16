//! The environment layer: `HARNESS_SECTION__KEY=value` variables.

use toml::{Table, Value};

/// Variables with the `HARNESS_` prefix that are not config keys.
const RESERVED: &[&str] = &["HARNESS_LOG"];

/// Builds a config layer from environment variables.
///
/// `HARNESS_POLICY__PROFILE=ci` becomes `policy.profile = "ci"`: the prefix is
/// removed, `__` separates path segments, and names are lowercased. Values are
/// parsed as TOML when possible (`true`, `42`, `["a"]`), otherwise taken as
/// strings. Variables without the prefix, and reserved ones such as
/// `HARNESS_LOG`, are ignored.
///
/// # Examples
/// ```
/// use harness_config::env_layer;
/// let layer = env_layer([("HARNESS_RUNTIME__MAX_PARALLEL_TOOLS".to_string(), "8".to_string())]);
/// assert_eq!(layer["runtime"]["max_parallel_tools"].as_integer(), Some(8));
/// ```
pub fn env_layer(vars: impl IntoIterator<Item = (String, String)>) -> Table {
    let mut out = Table::new();
    for (name, raw) in vars {
        if RESERVED.contains(&name.as_str()) {
            continue;
        }
        let Some(rest) = name.strip_prefix("HARNESS_") else {
            continue;
        };
        let path: Vec<String> = rest.split("__").map(str::to_ascii_lowercase).collect();
        if path.iter().any(String::is_empty) {
            continue;
        }
        insert_path(&mut out, &path, parse_value(&raw));
    }
    out
}

/// Inserts `value` at `path`, creating (or replacing non-table values with) tables on the way.
fn insert_path(table: &mut Table, path: &[String], value: Value) {
    match path {
        [] => {}
        [last] => {
            table.insert(last.clone(), value);
        }
        [head, tail @ ..] => {
            let entry = table
                .entry(head.clone())
                .or_insert_with(|| Value::Table(Table::new()));
            if !entry.is_table() {
                *entry = Value::Table(Table::new());
            }
            if let Value::Table(inner) = entry {
                insert_path(inner, tail, value);
            }
        }
    }
}

/// Parses an environment value as a TOML value, falling back to a string.
fn parse_value(raw: &str) -> Value {
    toml::from_str::<Table>(&format!("v = {raw}"))
        .ok()
        .and_then(|mut t| t.remove("v"))
        .unwrap_or_else(|| Value::String(raw.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn layer(pairs: &[(&str, &str)]) -> Table {
        env_layer(
            pairs
                .iter()
                .map(|(k, v)| ((*k).to_owned(), (*v).to_owned())),
        )
    }

    #[test]
    fn double_underscore_separates_sections() {
        let l = layer(&[("HARNESS_POLICY__PROFILE", "ci")]);
        assert_eq!(l["policy"]["profile"].as_str(), Some("ci"));
    }

    #[test]
    fn single_segment_names_are_top_level_keys() {
        let l = layer(&[("HARNESS_DEFAULT_PERSONA", "coder")]);
        assert_eq!(l["default_persona"].as_str(), Some("coder"));
    }

    #[test]
    fn values_parse_as_toml_when_possible() {
        let l = layer(&[
            ("HARNESS_SANDBOX__NETWORK", "true"),
            ("HARNESS_CONTEXT__COMPACT_AT_TOKENS", "100000"),
            ("HARNESS_POLICY__DEFAULT_DENY_PATHS", "[\"a\", \"b\"]"),
        ]);
        assert_eq!(l["sandbox"]["network"].as_bool(), Some(true));
        assert_eq!(
            l["context"]["compact_at_tokens"].as_integer(),
            Some(100_000)
        );
        assert_eq!(
            l["policy"]["default_deny_paths"].as_array().map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn ignores_unprefixed_and_reserved_variables() {
        let l = layer(&[
            ("PATH", "/bin"),
            ("HARNESS_LOG", "debug"),
            ("HARNESS_", "x"),
        ]);
        assert!(l.is_empty(), "{l:?}");
    }

    #[test]
    fn parse_value_keeps_unparseable_text_as_string() {
        assert_eq!(
            parse_value("hello world"),
            Value::String("hello world".into())
        );
        assert_eq!(parse_value("1.5"), Value::Float(1.5));
    }
}
