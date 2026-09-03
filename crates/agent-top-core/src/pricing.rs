//! Static price table, USD per million tokens.
//!
//! Anthropic prices cached 2026-06-24 from the published price list. Cache
//! writes are 1.25x input for the 5-minute TTL and 2x for the 1-hour TTL on
//! every model; cache reads are 0.1x input except Claude Fable 5.1 (0.025x).
//! OpenAI, Google and other vendors are not priced here: their tokens are
//! counted but reported as "unpriced" until a user-supplied table exists
//! (see RFC-103 in the internal handbook).

use crate::model::TokenUsage;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl Price {
    const fn anthropic(input: f64, output: f64, cache_read: f64) -> Price {
        Price { input, output, cache_write_5m: input * 1.25, cache_write_1h: input * 2.0, cache_read }
    }

    /// Cost in USD of `usage` at this price.
    pub fn cost(&self, usage: &TokenUsage) -> f64 {
        const M: f64 = 1_000_000.0;
        usage.input as f64 * self.input / M
            + usage.cache_write_5m as f64 * self.cache_write_5m / M
            + usage.cache_write_1h as f64 * self.cache_write_1h / M
            + usage.cache_read as f64 * self.cache_read / M
            + usage.output as f64 * self.output / M
    }
}

/// Longest-prefix table. Order matters: `claude-fable-5-1` must precede `claude-fable-5`.
const TABLE: &[(&str, Price)] = &[
    ("claude-fable-5-1", Price::anthropic(10.0, 50.0, 0.25)),
    ("claude-mythos-5-1", Price::anthropic(10.0, 50.0, 0.25)),
    ("claude-fable-5", Price::anthropic(10.0, 50.0, 1.0)),
    ("claude-mythos-5", Price::anthropic(10.0, 50.0, 1.0)),
    ("claude-opus-5", Price::anthropic(5.0, 25.0, 0.5)),
    ("claude-opus-4-8", Price::anthropic(5.0, 25.0, 0.5)),
    ("claude-opus-4-7", Price::anthropic(5.0, 25.0, 0.5)),
    ("claude-opus-4-6", Price::anthropic(5.0, 25.0, 0.5)),
    ("claude-sonnet-5", Price::anthropic(2.0, 10.0, 0.2)),
    ("claude-sonnet-4-6", Price::anthropic(3.0, 15.0, 0.3)),
    ("claude-haiku-4-5", Price::anthropic(1.0, 5.0, 0.1)),
];

/// Look up a price by model id. Date-suffixed ids (`claude-sonnet-4-6-20251114`)
/// and vendor-prefixed ids (`anthropic.claude-opus-5`) resolve to the base model.
pub fn price_for(model: &str) -> Option<Price> {
    let m = model.trim().to_ascii_lowercase();
    let m = m.strip_prefix("anthropic.").unwrap_or(&m);
    let m = m.strip_prefix("us.anthropic.").unwrap_or(m);
    TABLE.iter().filter(|(prefix, _)| m.starts_with(prefix)).max_by_key(|(prefix, _)| prefix.len()).map(|(_, p)| *p)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn longest_prefix_wins() {
        assert_eq!(price_for("claude-fable-5-1").unwrap().cache_read, 0.25);
        assert_eq!(price_for("claude-fable-5").unwrap().cache_read, 1.0);
        assert_eq!(price_for("claude-sonnet-4-6-20251114").unwrap().input, 3.0);
        assert!(price_for("gpt-5-codex").is_none());
        assert!(price_for("<synthetic>").is_none());
    }

    #[test]
    fn cost_arithmetic() {
        let p = price_for("claude-sonnet-5").unwrap();
        let u = TokenUsage { input: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 12.0).abs() < 1e-9);
        let u = TokenUsage { cache_write_1h: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 4.0).abs() < 1e-9);
    }
}
