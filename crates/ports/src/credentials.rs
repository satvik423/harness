//! [`ApiKey`]: a provider credential that never shows up in logs or errors.

use std::fmt;

/// A provider API key. `Debug` prints `ApiKey(***)`; the key is only reachable
/// through [`ApiKey::expose`], which adapters call when building a request header.
///
/// # Examples
/// ```
/// use harness_ports::ApiKey;
/// let key = ApiKey::new("sk-example");
/// assert_eq!(format!("{key:?}"), "ApiKey(***)");
/// assert_eq!(key.expose(), "sk-example");
/// ```
#[derive(Clone, PartialEq, Eq)]
pub struct ApiKey(String);

impl ApiKey {
    /// Wraps a key.
    #[must_use]
    pub fn new(key: impl Into<String>) -> Self {
        Self(key.into())
    }

    /// The key itself. Use it only to build an authentication header.
    #[must_use]
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ApiKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ApiKey(***)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_never_shows_the_key() {
        let key = ApiKey::new("sk-secret-value");
        assert!(!format!("{key:?}").contains("secret"));
        assert_eq!(key.expose(), "sk-secret-value");
    }
}
