//! `agent-top sql`: print a query against the history store as a table,
//! JSON or CSV.

use agent_top_store::{Described, QueryResult, Value};
use serde_json::json;

/// Numbers right-aligned, text left, a rule under the header, a row count.
pub fn to_table(r: &QueryResult) -> String {
    let cells: Vec<Vec<String>> = r.rows.iter().map(|row| row.iter().map(plain).collect()).collect();
    let numeric: Vec<bool> =
        (0..r.columns.len()).map(|i| r.rows.iter().all(|row| matches!(row[i], Value::Integer(_) | Value::Real(_) | Value::Null))).collect();
    let widths: Vec<usize> = r
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| cells.iter().map(|row| row[i].chars().count()).chain([c.chars().count()]).max().unwrap_or(0))
        .collect();
    let line = |vals: &[String]| {
        let parts: Vec<String> = vals
            .iter()
            .enumerate()
            .map(|(i, v)| if numeric[i] { format!("{v:>w$}", w = widths[i]) } else { format!("{v:<w$}", w = widths[i]) })
            .collect();
        parts.join("  ").trim_end().to_string()
    };
    let mut out = String::new();
    out.push_str(&line(&r.columns));
    out.push('\n');
    out.push_str(&widths.iter().map(|w| "-".repeat(*w)).collect::<Vec<_>>().join("  "));
    out.push('\n');
    for row in &cells {
        out.push_str(&line(row));
        out.push('\n');
    }
    let n = r.rows.len();
    out.push_str(&format!("({n} row{})\n", if n == 1 { "" } else { "s" }));
    out
}

/// An array of objects, one per row, keyed by column name.
pub fn to_json(r: &QueryResult) -> serde_json::Value {
    let rows: Vec<serde_json::Value> = r
        .rows
        .iter()
        .map(|row| {
            let obj: serde_json::Map<String, serde_json::Value> = r
                .columns
                .iter()
                .zip(row)
                .map(|(c, v)| {
                    let v = match v {
                        Value::Null => serde_json::Value::Null,
                        Value::Integer(i) => json!(i),
                        Value::Real(f) => json!(f),
                        Value::Text(s) => json!(s),
                        Value::Blob(n) => json!(format!("<{n} bytes>")),
                    };
                    (c.clone(), v)
                })
                .collect();
            serde_json::Value::Object(obj)
        })
        .collect();
    serde_json::Value::Array(rows)
}

/// RFC 4180: a header row, fields quoted when they hold a comma, quote or newline.
pub fn to_csv(r: &QueryResult) -> String {
    let field = |s: &str| {
        if s.contains([',', '"', '\n', '\r']) { format!("\"{}\"", s.replace('"', "\"\"")) } else { s.to_string() }
    };
    let mut out = String::new();
    out.push_str(&r.columns.iter().map(|c| field(c)).collect::<Vec<_>>().join(","));
    out.push_str("\r\n");
    for row in &r.rows {
        out.push_str(&row.iter().map(|v| field(&plain(v))).collect::<Vec<_>>().join(","));
        out.push_str("\r\n");
    }
    out
}

/// Each table and view, what it holds, and its columns.
pub fn schema(tables: &[Described]) -> String {
    let mut out = String::new();
    for t in tables {
        out.push_str(&format!("{} ({}): {}\n", t.name, t.kind, t.about));
        let cols: Vec<String> = t
            .columns
            .iter()
            .map(|c| if c.ty.is_empty() { c.name.clone() } else { format!("{} {}", c.name, c.ty.to_lowercase()) })
            .collect();
        out.push_str(&format!("  {}\n\n", cols.join(", ")));
    }
    out
}

fn plain(v: &Value) -> String {
    match v {
        Value::Null => String::new(),
        Value::Integer(i) => i.to_string(),
        Value::Real(f) => real(*f),
        Value::Text(s) => s.clone(),
        Value::Blob(n) => format!("<{n} bytes>"),
    }
}

/// At most six decimals, without trailing zeros: `1697.53`, not `1697.5300000001`.
fn real(f: f64) -> String {
    let s = format!("{f:.6}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" { "0".into() } else { s.to_string() }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> QueryResult {
        QueryResult {
            columns: vec!["project".into(), "cost_usd".into()],
            rows: vec![vec![Value::Text("code/app".into()), Value::Real(12.5)], vec![Value::Text("a, \"b\"".into()), Value::Null]],
        }
    }

    #[test]
    fn table_aligns_numbers_right_and_counts_rows() {
        assert_eq!(to_table(&result()), "project   cost_usd\n--------  --------\ncode/app      12.5\na, \"b\"\n(2 rows)\n");
    }

    #[test]
    fn csv_quotes_what_needs_it() {
        assert_eq!(to_csv(&result()), "project,cost_usd\r\ncode/app,12.5\r\n\"a, \"\"b\"\"\",\r\n");
    }

    #[test]
    fn json_keeps_types() {
        assert_eq!(to_json(&result()), json!([{ "project": "code/app", "cost_usd": 12.5 }, { "project": "a, \"b\"", "cost_usd": null }]));
    }

    #[test]
    fn reals_lose_float_noise() {
        assert_eq!(real(1697.5300000001), "1697.53");
        assert_eq!(real(3.0), "3");
        assert_eq!(real(-0.0000001), "0");
    }
}
