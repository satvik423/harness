//! Deep merge of TOML tables (how config layers combine).

use toml::{Table, Value};

/// Merges `overlay` into `base`.
///
/// Tables merge key by key, recursively. Any other value in `overlay`
/// (string, number, bool, array, datetime) replaces the value in `base`;
/// arrays are replaced, not concatenated, so a layer can shorten a list.
pub fn deep_merge(base: &mut Table, overlay: Table) {
    for (key, value) in overlay {
        match (base.get_mut(&key), value) {
            (Some(Value::Table(existing)), Value::Table(incoming)) => {
                deep_merge(existing, incoming)
            }
            (_, value) => {
                base.insert(key, value);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Table {
        toml::from_str(s).unwrap()
    }

    #[test]
    fn scalars_in_overlay_win() {
        let mut base = t("a = 1\nb = 2");
        deep_merge(&mut base, t("b = 3"));
        assert_eq!(base, t("a = 1\nb = 3"));
    }

    #[test]
    fn nested_tables_merge_key_by_key() {
        let mut base = t("[p]\nx = 1\ny = 2\n[p.q]\nz = 1");
        deep_merge(&mut base, t("[p]\ny = 9\n[p.q]\nw = 2"));
        assert_eq!(base, t("[p]\nx = 1\ny = 9\n[p.q]\nz = 1\nw = 2"));
    }

    #[test]
    fn arrays_are_replaced_not_concatenated() {
        let mut base = t("v = [1, 2, 3]");
        deep_merge(&mut base, t("v = [4]"));
        assert_eq!(base, t("v = [4]"));
    }

    #[test]
    fn a_table_can_replace_a_scalar_and_vice_versa() {
        let mut base = t("a = 1\n[b]\nc = 1");
        deep_merge(&mut base, t("b = 2\n[a]\nd = 1"));
        assert_eq!(base, t("b = 2\n[a]\nd = 1"));
    }
}
