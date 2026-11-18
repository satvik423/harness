//! [`ModelPrice`]: turning a turn's token usage into a cost (FR-PRV-06).

use harness_types::Usage;

/// A model's price in US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ModelPrice {
    /// Uncached input.
    pub input: f64,
    /// Output.
    pub output: f64,
    /// Input read from the cache.
    pub cache_read: f64,
    /// Input written to the cache.
    pub cache_write: f64,
}

impl ModelPrice {
    /// The cost of `usage` in millionths of a dollar (a dollar per million
    /// tokens is one millionth of a dollar per token).
    ///
    /// # Examples
    /// ```
    /// use harness_runtime::ModelPrice;
    /// use harness_types::Usage;
    /// let price = ModelPrice { input: 3.0, output: 15.0, cache_read: 0.3, cache_write: 3.75 };
    /// let usage = Usage { input: 1_000, output: 100, cache_read: 10_000, cache_write: 0 };
    /// assert_eq!(price.cost_micros(&usage), 3_000 + 1_500 + 3_000);
    /// ```
    #[must_use]
    pub fn cost_micros(&self, usage: &Usage) -> u64 {
        #[allow(clippy::cast_precision_loss)]
        let micros = usage.input as f64 * self.input
            + usage.output as f64 * self.output
            + usage.cache_read as f64 * self.cache_read
            + usage.cache_write as f64 * self.cache_write;
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        {
            micros.max(0.0).round() as u64
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_kind_of_token_is_priced() {
        let p = ModelPrice {
            input: 1.0,
            output: 2.0,
            cache_read: 0.5,
            cache_write: 4.0,
        };
        let u = Usage {
            input: 10,
            output: 10,
            cache_read: 10,
            cache_write: 10,
        };
        assert_eq!(p.cost_micros(&u), 75);
        assert_eq!(p.cost_micros(&Usage::default()), 0);
    }
}
