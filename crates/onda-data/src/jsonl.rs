//! JSONL (newline-delimited JSON) record-view engine (onda W28).
//!
//! Per-line, lazy operations so the editor never parses beyond the viewport — a 10GB
//! file streams. Each line is one record: parse it, summarize it for the folded view,
//! pretty/minify it for `:record-edit` (key order preserved via serde_json's
//! `preserve_order`), and sample keys for the `:fields` schema overlay. Parse errors
//! become per-record diagnostics; the file stays editable as plain text.

use serde_json::Value;

/// A parse error for one record line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordDiagnostic {
    /// 0-based line number.
    pub line: usize,
    pub message: String,
}

/// Parse a single JSONL line into a `Value`.
pub fn parse_record(line: &str) -> Result<Value, serde_json::Error> {
    serde_json::from_str(line.trim())
}

/// Return a diagnostic if `line` (non-blank) fails to parse as JSON.
pub fn record_diagnostic(line_no: usize, line: &str) -> Option<RecordDiagnostic> {
    if line.trim().is_empty() {
        return None;
    }
    match parse_record(line) {
        Ok(_) => None,
        Err(e) => Some(RecordDiagnostic {
            line: line_no,
            message: format!("line {}: {}", line_no + 1, e),
        }),
    }
}

/// One-line summary of a record for the folded view: the first `k` object fields as
/// `key=value`, or a short description for non-objects.
pub fn summary(value: &Value, k: usize) -> String {
    match value {
        Value::Object(map) => {
            let parts: Vec<String> = map
                .iter()
                .take(k)
                .map(|(key, v)| format!("{key}={}", scalar_repr(v)))
                .collect();
            let more = map.len().saturating_sub(k);
            let mut s = parts.join("  ");
            if more > 0 {
                s.push_str(&format!("  … (+{more})"));
            }
            s
        }
        Value::Array(a) => format!("[{} items]", a.len()),
        other => scalar_repr(other),
    }
}

/// Compact representation of a scalar/value for summaries (truncated).
fn scalar_repr(v: &Value) -> String {
    let s = match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => format!("[{}]", a.len()),
        Value::Object(o) => format!("{{{}}}", o.len()),
    };
    const MAX: usize = 40;
    if s.chars().count() > MAX {
        let truncated: String = s.chars().take(MAX).collect();
        format!("{truncated}…")
    } else {
        s
    }
}

/// Pretty-print a record (for `:record-edit`), preserving key order.
pub fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_default()
}

/// Minify a record back to a single JSONL line, preserving key order.
pub fn minify(value: &Value) -> String {
    serde_json::to_string(value).unwrap_or_default()
}

/// Round-trip a JSONL line through pretty → minify. Returns the minified result.
/// With `preserve_order`, object key order is preserved end-to-end.
pub fn round_trip(line: &str) -> Result<String, serde_json::Error> {
    let value = parse_record(line)?;
    let pretty = pretty(&value);
    let reparsed: Value = serde_json::from_str(&pretty)?;
    Ok(minify(&reparsed))
}

/// JSON value type, for schema reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JsonType {
    Null,
    Bool,
    Number,
    String,
    Array,
    Object,
}

impl JsonType {
    pub fn of(v: &Value) -> Self {
        match v {
            Value::Null => JsonType::Null,
            Value::Bool(_) => JsonType::Bool,
            Value::Number(_) => JsonType::Number,
            Value::String(_) => JsonType::String,
            Value::Array(_) => JsonType::Array,
            Value::Object(_) => JsonType::Object,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            JsonType::Null => "null",
            JsonType::Bool => "bool",
            JsonType::Number => "number",
            JsonType::String => "string",
            JsonType::Array => "array",
            JsonType::Object => "object",
        }
    }
}

/// Per-field statistics for the `:fields` overlay.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldStat {
    pub key: String,
    /// How many sampled records contained this key.
    pub count: usize,
    /// Observed types and their counts, in first-seen order.
    pub types: Vec<(JsonType, usize)>,
}

/// Build the union-of-keys schema across up to `sample_n` parseable record lines.
/// Keys appear in first-seen order (stable schema feel for unknown datasets).
pub fn field_schema<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    sample_n: usize,
) -> Vec<FieldStat> {
    let mut stats: Vec<FieldStat> = Vec::new();
    let mut sampled = 0usize;
    for line in lines {
        if sampled >= sample_n {
            break;
        }
        let value = match parse_record(line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Value::Object(map) = value {
            sampled += 1;
            for (key, v) in map.iter() {
                let ty = JsonType::of(v);
                if let Some(stat) = stats.iter_mut().find(|s| s.key == *key) {
                    stat.count += 1;
                    if let Some(entry) = stat.types.iter_mut().find(|(t, _)| *t == ty) {
                        entry.1 += 1;
                    } else {
                        stat.types.push((ty, 1));
                    }
                } else {
                    stats.push(FieldStat {
                        key: key.clone(),
                        count: 1,
                        types: vec![(ty, 1)],
                    });
                }
            }
        }
    }
    stats
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_record() {
        let v = parse_record(r#"{"a":1,"b":"x"}"#).unwrap();
        assert_eq!(v["a"], 1);
        assert_eq!(v["b"], "x");
    }

    #[test]
    fn diagnostic_on_bad_record() {
        let d = record_diagnostic(4, "{not json}").unwrap();
        assert_eq!(d.line, 4);
        assert!(d.message.contains("line 5"));
    }

    #[test]
    fn blank_line_no_diagnostic() {
        assert!(record_diagnostic(0, "   ").is_none());
    }

    #[test]
    fn summary_first_k_fields() {
        let v = parse_record(r#"{"id":1,"name":"Alice","age":30,"city":"NYC"}"#).unwrap();
        let s = summary(&v, 2);
        assert!(s.starts_with("id=1  name=Alice"));
        assert!(s.contains("+2"));
    }

    #[test]
    fn summary_array_and_scalar() {
        assert_eq!(summary(&parse_record("[1,2,3]").unwrap(), 3), "[3 items]");
        assert_eq!(summary(&parse_record("42").unwrap(), 3), "42");
    }

    #[test]
    fn round_trip_preserves_key_order() {
        // Keys deliberately not alphabetical — order must survive pretty→minify.
        let line = r#"{"zebra":1,"apple":2,"mango":3}"#;
        let out = round_trip(line).unwrap();
        assert_eq!(out, r#"{"zebra":1,"apple":2,"mango":3}"#);
    }

    #[test]
    fn round_trip_semantically_identical() {
        let cases = [
            r#"{"a":1,"nested":{"x":[1,2,{"y":true}]},"s":"hi"}"#,
            r#"{"unicode":"日本語","n":3.14,"b":false,"nil":null}"#,
            r#"[1,2,3,{"k":"v"}]"#,
        ];
        for line in cases {
            let original: Value = parse_record(line).unwrap();
            let round: Value = parse_record(&round_trip(line).unwrap()).unwrap();
            assert_eq!(original, round, "semantic identity for {line}");
        }
    }

    #[test]
    fn pretty_is_multiline_minify_is_single() {
        let v = parse_record(r#"{"a":1,"b":2}"#).unwrap();
        assert!(pretty(&v).contains('\n'));
        assert!(!minify(&v).contains('\n'));
    }

    #[test]
    fn schema_unions_keys_with_types() {
        let lines = [
            r#"{"id":1,"name":"Alice"}"#,
            r#"{"id":2,"name":"Bob","active":true}"#,
            r#"{"id":"x3","name":"Cara"}"#, // id sometimes string
            "garbage",                      // skipped
        ];
        let schema = field_schema(lines, 100);
        let id = schema.iter().find(|s| s.key == "id").unwrap();
        assert_eq!(id.count, 3);
        // id seen as Number twice and String once.
        let num = id
            .types
            .iter()
            .find(|(t, _)| *t == JsonType::Number)
            .unwrap();
        assert_eq!(num.1, 2);
        let s = id
            .types
            .iter()
            .find(|(t, _)| *t == JsonType::String)
            .unwrap();
        assert_eq!(s.1, 1);

        let active = schema.iter().find(|s| s.key == "active").unwrap();
        assert_eq!(active.count, 1);
        assert_eq!(active.types[0].0, JsonType::Bool);

        // First-seen order: id, name, active.
        let keys: Vec<&str> = schema.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, vec!["id", "name", "active"]);
    }

    #[test]
    fn schema_respects_sample_cap() {
        let lines: Vec<String> = (0..1000)
            .map(|i| format!(r#"{{"only_in_{}":{}}}"#, i, i))
            .collect();
        let refs: Vec<&str> = lines.iter().map(|s| s.as_str()).collect();
        let schema = field_schema(refs, 5);
        // Only 5 records sampled → at most 5 distinct keys.
        assert_eq!(schema.len(), 5);
    }
}
