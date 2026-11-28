//! Energy and CO2e estimates from a coefficient table the user writes.
//!
//! No provider publishes energy per token for the models agent-top prices, so
//! nothing here is built in (RFC-111, DEC-030). A table at
//! `$AGENT_TOP_ENERGY`, `$XDG_CONFIG_HOME/agent-top/energy.toml` or
//! `~/.config/agent-top/energy.toml` gives watt-hours per million tokens for
//! each kind of token, per model, and the grid's grams of CO2e per kWh. Every
//! entry names its source and the date it was checked, and any figure may be a
//! `[low, high]` range, which carries through to the result.
//!
//! ```toml
//! [grid]
//! gco2e_per_kwh = [94, 420]
//! source = "..."
//! checked = "2026-11-28"
//!
//! [models."claude-opus-5-5"]
//! input = [0, 390]
//! cache_write = [0, 490]
//! cache_read = [0, 39]
//! output = [1950, 28000]
//! source = "..."
//! checked = "2026-11-28"
//! ```
//!
//! Model ids match exactly, after trimming and lowercasing; there is no prefix
//! fallback, since a near match would apply one model's figure to another. A
//! model with no entry is never estimated: its tokens are counted as
//! unestimated, as unpriced tokens are for cost.

use crate::model::TokenUsage;
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;

/// A figure known only to lie between two bounds. A single number is a range
/// with equal bounds.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Range {
    pub low: f64,
    pub high: f64,
}

impl Range {
    pub const ZERO: Range = Range { low: 0.0, high: 0.0 };

    pub fn plus(self, other: Range) -> Range {
        Range { low: self.low + other.low, high: self.high + other.high }
    }

    fn scale(self, k: f64) -> Range {
        Range { low: self.low * k, high: self.high * k }
    }

    /// Both bounds multiplied by the matching bound of `other`: the lowest
    /// energy at the cleanest grid, the highest at the dirtiest.
    fn times(self, other: Range) -> Range {
        Range { low: self.low * other.low, high: self.high * other.high }
    }
}

impl<'de> Deserialize<'de> for Range {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Range, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            One(f64),
            Two([f64; 2]),
        }
        let (low, high) = match Raw::deserialize(d)? {
            Raw::One(v) => (v, v),
            Raw::Two([a, b]) => (a, b),
        };
        if !(low.is_finite() && high.is_finite()) || low < 0.0 || high < low {
            return Err(serde::de::Error::custom(format!("{low}..{high} is not a range: both bounds must be 0 or more, low first")));
        }
        Ok(Range { low, high })
    }
}

/// Watt-hours per million tokens of each kind, for one model.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Coefficients {
    pub input: Range,
    pub cache_write: Range,
    pub cache_read: Range,
    pub output: Range,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelEntry {
    input: Range,
    cache_write: Range,
    cache_read: Range,
    output: Range,
    source: String,
    checked: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct GridEntry {
    gco2e_per_kwh: Range,
    source: String,
    checked: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    grid: GridEntry,
    #[serde(default)]
    models: BTreeMap<String, ModelEntry>,
}

/// One model's entry as loaded, with where it came from.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub model: String,
    pub coefficients: Coefficients,
    pub source: String,
    pub checked: String,
}

/// The loaded table. Empty when there is no file or the file was refused, in
/// which case `warnings` says why.
#[derive(Debug, Default)]
pub struct Table {
    pub path: Option<PathBuf>,
    pub entries: Vec<Entry>,
    pub gco2e_per_kwh: Option<Range>,
    pub grid_source: Option<String>,
    pub grid_checked: Option<String>,
    pub warnings: Vec<String>,
}

/// What a set of sessions came to, summed. Tokens on models with no entry are
/// in `unestimated_tokens` and in none of the ranges.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Estimate {
    pub energy_wh: Range,
    pub co2e_g: Range,
    /// The part of `energy_wh` that came from cache reads.
    pub cache_read_wh: Range,
    pub estimated_tokens: u64,
    pub unestimated_tokens: u64,
}

impl Range {
    pub fn to_json(self) -> serde_json::Value {
        serde_json::json!({ "low": self.low, "high": self.high })
    }
}

impl Estimate {
    /// The shape `report --json` and the web API both use, CO2e first.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "estimated": true,
            "co2e_g": self.co2e_g.to_json(),
            "energy_wh": self.energy_wh.to_json(),
            "cache_read_wh": self.cache_read_wh.to_json(),
            "estimated_tokens": self.estimated_tokens,
            "unestimated_tokens": self.unestimated_tokens,
        })
    }

    pub fn add(&mut self, other: &Estimate) {
        self.energy_wh = self.energy_wh.plus(other.energy_wh);
        self.co2e_g = self.co2e_g.plus(other.co2e_g);
        self.cache_read_wh = self.cache_read_wh.plus(other.cache_read_wh);
        self.estimated_tokens += other.estimated_tokens;
        self.unestimated_tokens += other.unestimated_tokens;
    }
}

impl Table {
    /// Whether there is anything to estimate with.
    pub fn is_usable(&self) -> bool {
        self.gco2e_per_kwh.is_some()
    }

    pub fn entry(&self, model: &str) -> Option<&Entry> {
        let m = model.trim().to_ascii_lowercase();
        self.entries.iter().find(|e| e.model == m)
    }

    /// The estimate for one session's tokens on one model.
    pub fn estimate(&self, model: Option<&str>, usage: &TokenUsage) -> Estimate {
        let tokens = usage.total();
        let (Some(grid), Some(e)) = (self.gco2e_per_kwh, model.and_then(|m| self.entry(m))) else {
            return Estimate { unestimated_tokens: tokens, ..Estimate::default() };
        };
        const M: f64 = 1_000_000.0;
        let c = e.coefficients;
        let cache_read_wh = c.cache_read.scale(usage.cache_read as f64 / M);
        let energy_wh = c
            .input
            .scale(usage.input as f64 / M)
            .plus(c.cache_write.scale(usage.cache_write() as f64 / M))
            .plus(cache_read_wh)
            .plus(c.output.scale(usage.output as f64 / M));
        Estimate {
            energy_wh,
            co2e_g: energy_wh.scale(1.0 / 1000.0).times(grid),
            cache_read_wh,
            estimated_tokens: tokens,
            unestimated_tokens: 0,
        }
    }
}

/// Build a table from a file's text. Pure, so every refusal is testable. A
/// file that does not parse, or whose entries lack a source or a date, is
/// refused whole: half a table would estimate some models and silently skip
/// others the user meant to include.
pub fn build(text: &str, path: PathBuf) -> Table {
    let mut t = Table { path: Some(path.clone()), ..Table::default() };
    let refuse = |t: &mut Table, why: String| t.warnings.push(format!("{}: ignored, {why}", path.display()));
    let f: File = match toml::from_str(text) {
        Ok(f) => f,
        Err(e) => {
            refuse(&mut t, first_line(&e.to_string()));
            return t;
        }
    };
    let mut problems = Vec::new();
    let mut check = |what: &str, source: &str, checked: &str| {
        if source.trim().is_empty() {
            problems.push(format!("{what} has no source"));
        }
        if !is_date(checked) {
            problems.push(format!("{what} has no checked date as YYYY-MM-DD"));
        }
    };
    check("[grid]", &f.grid.source, &f.grid.checked);
    for (model, m) in &f.models {
        check(&format!("[models.\"{model}\"]"), &m.source, &m.checked);
    }
    if !problems.is_empty() {
        refuse(&mut t, problems.join("; "));
        return t;
    }
    t.gco2e_per_kwh = Some(f.grid.gco2e_per_kwh);
    t.grid_source = Some(f.grid.source);
    t.grid_checked = Some(f.grid.checked);
    t.entries = f
        .models
        .into_iter()
        .map(|(model, m)| Entry {
            model: model.trim().to_ascii_lowercase(),
            coefficients: Coefficients { input: m.input, cache_write: m.cache_write, cache_read: m.cache_read, output: m.output },
            source: m.source,
            checked: m.checked,
        })
        .collect();
    t
}

fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 10 && b[4] == b'-' && b[7] == b'-' && b.iter().enumerate().all(|(i, c)| i == 4 || i == 7 || c.is_ascii_digit())
}

/// A TOML error in one line: where it is, then why. The text between is the
/// quoted source line and a caret, which a one-line warning cannot show.
fn first_line(s: &str) -> String {
    let mut lines = s.lines().map(str::trim).filter(|l| !l.is_empty());
    let head = lines.next().unwrap_or(s).to_string();
    match lines.next_back() {
        Some(why) if !why.starts_with('|') && !why.contains(" | ") => format!("{head}: {why}"),
        _ => head,
    }
}

/// `$AGENT_TOP_ENERGY` wins, then the XDG config directory, then `~/.config`.
pub fn user_energy_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("AGENT_TOP_ENERGY") {
        return Some(PathBuf::from(p));
    }
    let dir = match std::env::var_os("XDG_CONFIG_HOME") {
        Some(d) => PathBuf::from(d),
        None => PathBuf::from(std::env::var_os("HOME")?).join(".config"),
    };
    Some(dir.join("agent-top").join("energy.toml"))
}

/// The user's table, read once. Empty, with no warning, when there is no file.
pub fn table() -> &'static Table {
    static TABLE: OnceLock<Table> = OnceLock::new();
    TABLE.get_or_init(|| match user_energy_path() {
        Some(p) => match std::fs::read_to_string(&p) {
            Ok(text) => build(&text, p),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Table { path: Some(p), ..Table::default() },
            Err(e) => Table { warnings: vec![format!("{}: ignored, {e}", p.display())], path: Some(p), ..Table::default() },
        },
        None => Table::default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const TABLE: &str = r#"
[grid]
gco2e_per_kwh = [100, 400]
source = "test grid"
checked = "2026-11-28"

[models."Claude-Opus-5-5"]
input = 390
cache_write = 490
cache_read = [0, 39]
output = [1950, 28000]
source = "test"
checked = "2026-11-28"
"#;

    fn table() -> Table {
        build(TABLE, PathBuf::from("/tmp/energy.toml"))
    }

    fn usage() -> TokenUsage {
        TokenUsage { input: 1_000_000, cache_write_5m: 1_000_000, cache_read: 10_000_000, output: 1_000_000, ..TokenUsage::default() }
    }

    #[test]
    fn a_session_is_estimated_from_its_model_s_coefficients() {
        let t = table();
        assert!(t.warnings.is_empty(), "{:?}", t.warnings);
        let e = t.estimate(Some("claude-opus-5-5"), &usage());
        // 390 + 490 + 10 × [0, 39] + [1950, 28000]
        assert_eq!(e.energy_wh, Range { low: 2830.0, high: 29270.0 });
        assert_eq!(e.cache_read_wh, Range { low: 0.0, high: 390.0 });
        // kWh × g/kWh: low with the clean grid, high with the dirty one.
        assert!((e.co2e_g.low - 283.0).abs() < 1e-9 && (e.co2e_g.high - 11708.0).abs() < 1e-9, "{:?}", e.co2e_g);
        assert_eq!(e.estimated_tokens, 13_000_000);
        assert_eq!(e.unestimated_tokens, 0);
    }

    #[test]
    fn a_model_with_no_entry_is_counted_not_estimated() {
        let t = table();
        // No prefix fallback: a dated or longer id is another model.
        for model in [Some("claude-opus-5-5-20261101"), Some("claude-opus-5"), Some("gpt-5.6-sol"), None] {
            let e = t.estimate(model, &usage());
            assert_eq!(e.energy_wh, Range::ZERO, "{model:?}");
            assert_eq!(e.unestimated_tokens, 13_000_000, "{model:?}");
        }
        assert!(t.estimate(Some("  CLAUDE-OPUS-5-5 "), &usage()).estimated_tokens > 0);
    }

    #[test]
    fn an_entry_without_a_source_or_date_refuses_the_whole_file() {
        let no_source = TABLE.replace("source = \"test\"", "source = \"\"");
        let t = build(&no_source, PathBuf::from("/tmp/energy.toml"));
        assert!(!t.is_usable());
        assert!(t.warnings[0].contains("[models.\"Claude-Opus-5-5\"] has no source"), "{:?}", t.warnings);

        let bad_date = TABLE.replace("checked = \"2026-11-28\"\n\n[models", "checked = \"last week\"\n\n[models");
        let t = build(&bad_date, PathBuf::from("/tmp/energy.toml"));
        assert!(t.warnings[0].contains("[grid] has no checked date"), "{:?}", t.warnings);
    }

    #[test]
    fn malformed_figures_are_refused() {
        for (bad, why) in [("output = [28000, 1950]", "not a range"), ("output = [-1, 5]", "not a range"), ("outptu = 5", "unknown field")]
        {
            let text = TABLE.replace("output = [1950, 28000]", bad);
            let t = build(&text, PathBuf::from("/tmp/energy.toml"));
            assert!(!t.is_usable(), "{bad}");
            assert!(t.warnings[0].contains(why) || bad.starts_with("outptu"), "{bad}: {:?}", t.warnings);
        }
        let no_grid = TABLE.split("[models").nth(1).map(|m| format!("[models{m}")).unwrap();
        assert!(!build(&no_grid, PathBuf::from("/tmp/e.toml")).is_usable());
    }

    #[test]
    fn estimates_add_up() {
        let t = table();
        let mut sum = Estimate::default();
        sum.add(&t.estimate(Some("claude-opus-5-5"), &usage()));
        sum.add(&t.estimate(Some("gpt-5.6-sol"), &usage()));
        assert_eq!(sum.estimated_tokens, 13_000_000);
        assert_eq!(sum.unestimated_tokens, 13_000_000);
        assert_eq!(sum.energy_wh.low, 2830.0);
    }
}
