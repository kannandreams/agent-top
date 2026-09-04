//! Model prices, loaded from data rather than compiled in as code.
//!
//! `prices.toml` next to this crate is embedded in the binary and is the
//! built-in table. A file at `$XDG_CONFIG_HOME/agent-top/prices.toml`, or
//! `~/.config/agent-top/prices.toml`, is merged over it at startup: an entry
//! with the same prefix replaces a built-in one, a new prefix is added. That
//! means a stale price can be corrected, and a model this project has never
//! heard of can be priced, without a release and without a Rust toolchain.
//!
//! A model with no entry is never guessed at. Its tokens are counted and
//! reported as unpriced, and any total containing them is shown as a floor.

use crate::model::TokenUsage;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::OnceLock;

/// The table shipped with the binary. See that file for the format.
const BUILTIN: &str = include_str!("../prices.toml");

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    pub output: f64,
    pub cache_write_5m: f64,
    pub cache_write_1h: f64,
    pub cache_read: f64,
}

impl Price {
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

/// Where an effective price came from, so `--prices` can show a user which of
/// their overrides actually took effect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Builtin,
    User,
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub prefix: String,
    pub price: Price,
    pub origin: Origin,
}

#[derive(Debug, Deserialize)]
struct FileTable {
    #[serde(default)]
    updated: Option<String>,
    #[serde(default)]
    model: Vec<FileModel>,
    #[serde(default)]
    server_tools: Option<FileServerTools>,
}

/// Server-side tools billed per call rather than per token.
#[derive(Debug, Deserialize)]
struct FileServerTools {
    /// USD per 1,000 web searches.
    web_search: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct FileModel {
    prefix: String,
    input: f64,
    output: f64,
    cache_read: f64,
    /// Anthropic charges 1.25x input for the 5 minute TTL and 2x for the hour.
    /// A vendor that prices cache writes differently sets these.
    cache_write_5m: Option<f64>,
    cache_write_1h: Option<f64>,
}

impl FileModel {
    fn price(&self) -> Price {
        Price {
            input: self.input,
            output: self.output,
            cache_read: self.cache_read,
            cache_write_5m: self.cache_write_5m.unwrap_or(self.input * 1.25),
            cache_write_1h: self.cache_write_1h.unwrap_or(self.input * 2.0),
        }
    }
}

/// The effective table, plus anything the user should be told about how it was
/// built. A bad user file must never take the built-in prices down with it, and
/// must never be swallowed either: it is reported and the built-ins stand.
#[derive(Debug, Clone, Default)]
pub struct Table {
    pub entries: Vec<Entry>,
    pub updated: Option<String>,
    pub user_path: Option<PathBuf>,
    pub warnings: Vec<String>,
    /// USD per 1,000 web searches, when the table prices them.
    pub web_search_per_1k: Option<f64>,
    pub web_search_origin: Option<Origin>,
}

impl Table {
    /// USD for `n` web searches, or zero when the table does not price them.
    /// Anthropic bills web search per search on top of the tokens it produces;
    /// web fetch and code execution alongside it are free (checked 2026-09-04).
    pub fn web_search_cost(&self, n: u64) -> f64 {
        self.web_search_per_1k.map(|p| n as f64 * p / 1_000.0).unwrap_or(0.0)
    }

    pub fn lookup(&self, model: &str) -> Option<Price> {
        let m = model.trim().to_ascii_lowercase();
        let m = m.strip_prefix("anthropic.").unwrap_or(&m);
        let m = m.strip_prefix("us.anthropic.").unwrap_or(m);
        self.entries.iter().filter(|e| m.starts_with(&e.prefix)).max_by_key(|e| e.prefix.len()).map(|e| e.price)
    }
}

fn parse(text: &str) -> Result<FileTable, toml::de::Error> {
    toml::from_str(text)
}

/// Build a table from the embedded text and an optional user file. Pure, so the
/// merge and every failure mode are testable without touching a real home
/// directory.
pub fn build(builtin: &str, user: Option<(&str, PathBuf)>) -> Table {
    let mut table = Table::default();
    match parse(builtin) {
        Ok(f) => {
            table.updated = f.updated;
            table.entries = f
                .model
                .into_iter()
                .map(|m| Entry { prefix: m.prefix.to_ascii_lowercase(), price: m.price(), origin: Origin::Builtin })
                .collect();
            if let Some(p) = f.server_tools.and_then(|t| t.web_search) {
                table.web_search_per_1k = Some(p);
                table.web_search_origin = Some(Origin::Builtin);
            }
        }
        // Only reachable if the shipped file is broken, which is a bug here
        // rather than anything a user can fix, but it must not panic.
        Err(e) => table.warnings.push(format!("built-in price table is invalid: {e}")),
    }

    let Some((text, path)) = user else { return table };
    table.user_path = Some(path.clone());
    match parse(text) {
        Ok(f) => {
            for m in f.model {
                let prefix = m.prefix.to_ascii_lowercase();
                let entry = Entry { prefix: prefix.clone(), price: m.price(), origin: Origin::User };
                match table.entries.iter().position(|e| e.prefix == prefix) {
                    Some(i) => table.entries[i] = entry,
                    None => table.entries.push(entry),
                }
            }
            if let Some(p) = f.server_tools.and_then(|t| t.web_search) {
                table.web_search_per_1k = Some(p);
                table.web_search_origin = Some(Origin::User);
            }
        }
        Err(e) => table.warnings.push(format!("{}: ignored, {}", path.display(), first_line(&e.to_string()))),
    }
    table
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or(s).to_string()
}

/// `$AGENT_TOP_PRICES` wins, then the XDG config directory, then `~/.config`.
pub fn user_price_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_TOP_PRICES") {
        return Some(PathBuf::from(p));
    }
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(dir.join("agent-top").join("prices.toml"))
}

/// The shipped table alone, with no user overrides merged in. Tests that
/// assert costs use this: a suite whose results depend on the developer's home
/// directory is worse than no suite.
pub fn builtin_table() -> &'static Table {
    static BUILTIN_TABLE: OnceLock<Table> = OnceLock::new();
    BUILTIN_TABLE.get_or_init(|| build(BUILTIN, None))
}

pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| {
        let user = user_price_path().and_then(|p| std::fs::read_to_string(&p).ok().map(|t| (t, p)));
        build(BUILTIN, user.as_ref().map(|(t, p)| (t.as_str(), p.clone())))
    })
}

/// Look up a price by model id. Date-suffixed ids (`claude-sonnet-4-6-20251114`)
/// and vendor-prefixed ids (`anthropic.claude-opus-5`) resolve to the base model.
pub fn price_for(model: &str) -> Option<Price> {
    table().lookup(model)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn builtin() -> Table {
        build(BUILTIN, None)
    }

    #[test]
    fn ships_a_valid_builtin_table() {
        let t = builtin();
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        assert_eq!(t.updated.as_deref(), Some("2026-06-24"));
        assert_eq!(t.web_search_per_1k, Some(10.0));
        assert!((t.web_search_cost(3) - 0.03).abs() < 1e-12);
        assert!(t.entries.len() >= 11);
        assert!(t.entries.iter().all(|e| e.origin == Origin::Builtin));
    }

    #[test]
    fn longest_prefix_wins() {
        let t = builtin();
        assert_eq!(t.lookup("claude-fable-5-1").unwrap().cache_read, 0.25);
        assert_eq!(t.lookup("claude-fable-5").unwrap().cache_read, 1.0);
        assert_eq!(t.lookup("claude-sonnet-4-6-20251114").unwrap().input, 3.0);
        assert_eq!(t.lookup("us.anthropic.claude-opus-5").unwrap().input, 5.0);
        assert!(t.lookup("gpt-5-codex").is_none());
        assert!(t.lookup("<synthetic>").is_none());
    }

    #[test]
    fn cost_arithmetic() {
        let t = builtin();
        let p = t.lookup("claude-sonnet-5").unwrap();
        let u = TokenUsage { input: 1_000_000, output: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 12.0).abs() < 1e-9);
        // Cache writes default to Anthropic's multipliers: 2x input for an hour.
        let u = TokenUsage { cache_write_1h: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 4.0).abs() < 1e-9);
        let u = TokenUsage { cache_write_5m: 1_000_000, ..Default::default() };
        assert!((p.cost(&u) - 2.5).abs() < 1e-9);
    }

    #[test]
    fn a_user_file_prices_a_new_model_and_corrects_a_stale_one() {
        let user = r#"
            [[model]]
            prefix = "gpt-5-codex"
            input = 1.25
            output = 10.0
            cache_read = 0.125

            [[model]]
            prefix = "claude-sonnet-5"
            input = 99.0
            output = 99.0
            cache_read = 9.0
        "#;
        let t = build(BUILTIN, Some((user, PathBuf::from("/tmp/prices.toml"))));
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);

        // A model the built-in table has never heard of is now priced.
        let p = t.lookup("gpt-5-codex-20260101").expect("new prefix is added");
        assert_eq!(p.input, 1.25);
        assert_eq!(p.cache_write_1h, 2.5, "cache writes still default off input");

        // A stale built-in price is replaced, not duplicated.
        assert_eq!(t.lookup("claude-sonnet-5").unwrap().input, 99.0);
        assert_eq!(t.entries.iter().filter(|e| e.prefix == "claude-sonnet-5").count(), 1);
        assert_eq!(t.entries.iter().filter(|e| e.origin == Origin::User).count(), 2);

        // Everything not overridden is untouched.
        assert_eq!(t.lookup("claude-opus-5").unwrap().input, 5.0);
    }

    #[test]
    fn explicit_cache_write_prices_win_over_the_anthropic_default() {
        let user = r#"
            [[model]]
            prefix = "some-vendor-model"
            input = 4.0
            output = 8.0
            cache_read = 0.4
            cache_write_5m = 0.0
            cache_write_1h = 0.0
        "#;
        let t = build(BUILTIN, Some((user, PathBuf::from("/tmp/p.toml"))));
        let p = t.lookup("some-vendor-model").unwrap();
        assert_eq!(p.cache_write_5m, 0.0, "a vendor that does not charge for cache writes can say so");
        assert_eq!(p.cache_write_1h, 0.0);
    }

    #[test]
    fn a_broken_user_file_is_reported_and_the_builtins_survive() {
        let t = build(BUILTIN, Some(("this is not toml {{{", PathBuf::from("/tmp/bad.toml"))));
        assert_eq!(t.lookup("claude-opus-5").unwrap().input, 5.0, "built-in prices must not go down with it");
        assert_eq!(t.warnings.len(), 1);
        assert!(t.warnings[0].contains("/tmp/bad.toml"), "{:?}", t.warnings);

        // A file that parses but is missing a required field is equally loud.
        let t = build(BUILTIN, Some(("[[model]]\nprefix = \"x\"\ninput = 1.0\n", PathBuf::from("/tmp/partial.toml"))));
        assert_eq!(t.warnings.len(), 1, "a missing price is not a zero price");
        assert!(t.lookup("x").is_none());
    }
}
