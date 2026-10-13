//! A deliberately tiny, safe templating and JSON-path layer for the custom
//! endpoint builder.
//!
//! **No code is ever evaluated.** Substitution is a string/value replacement
//! only. This prevents user-supplied templates from becoming an execution
//! vector while still allowing nonstandard JSON APIs to be described.

use serde_json::Value;

/// Resolve a dotted path against a JSON value.
///
/// * `"choices.0.message.content"` walks object keys and array indices.
/// * Numeric segments index arrays.
/// * Returns `None` if any segment is missing.
pub fn json_path<'a>(root: &'a Value, path: &str) -> Option<&'a Value> {
    if path.is_empty() {
        return Some(root);
    }
    let mut cur = root;
    for seg in path.split('.') {
        if seg.is_empty() {
            continue;
        }
        cur = match cur {
            Value::Object(map) => map.get(seg)?,
            Value::Array(arr) => {
                let idx: usize = seg.parse().ok()?;
                arr.get(idx)?
            }
            _ => return None,
        };
    }
    Some(cur)
}

/// Resolve a dotted path to an owned string, if the leaf is scalar-ish.
pub fn json_path_string(root: &Value, path: &str) -> Option<String> {
    match json_path(root, path)? {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        Value::Null => None,
        other => Some(other.to_string()),
    }
}

/// Render a template value by substituting `{{name}}` placeholders.
///
/// * If a string is *exactly* one placeholder (`"{{messages}}"`), the raw JSON
///   value is substituted, preserving type (array/object/number/bool).
/// * Otherwise placeholders are string-interpolated.
/// * Unknown placeholders are left verbatim so misconfiguration is visible.
pub fn render(template: &Value, vars: &dyn Fn(&str) -> Option<Value>) -> Value {
    match template {
        Value::String(s) => render_string(s, vars),
        Value::Array(items) => Value::Array(items.iter().map(|v| render(v, vars)).collect()),
        Value::Object(map) => {
            let mut out = serde_json::Map::with_capacity(map.len());
            for (k, v) in map {
                out.insert(k.clone(), render(v, vars));
            }
            Value::Object(out)
        }
        other => other.clone(),
    }
}

fn render_string(s: &str, vars: &dyn Fn(&str) -> Option<Value>) -> Value {
    if let Some(inner) = exact_placeholder(s) {
        if let Some(v) = vars(inner) {
            return v;
        }
        return Value::String(s.to_string());
    }
    // Interpolate embedded placeholders.
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i..].starts_with(b"{{") {
            if let Some(end) = s[i + 2..].find("}}") {
                let name = &s[i + 2..i + 2 + end];
                match vars(name) {
                    Some(Value::String(v)) => out.push_str(&v),
                    Some(v) => out.push_str(&v.to_string()),
                    None => {
                        out.push_str("{{");
                        out.push_str(name);
                        out.push_str("}}");
                    }
                }
                i = i + 2 + end + 2;
                continue;
            }
        }
        // copy one char (handle utf8 by pushing the char)
        let ch = s[i..].chars().next().unwrap();
        out.push(ch);
        i += ch.len_utf8();
    }
    Value::String(out)
}

fn exact_placeholder(s: &str) -> Option<&str> {
    let t = s.trim();
    t.strip_prefix("{{")
        .and_then(|r| r.strip_suffix("}}"))
        .map(|inner| inner.trim())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn json_path_walks_objects_and_arrays() {
        let v = json!({"choices":[{"message":{"content":"hi"}}]});
        assert_eq!(
            json_path_string(&v, "choices.0.message.content").as_deref(),
            Some("hi")
        );
        assert!(json_path(&v, "choices.5.message").is_none());
        assert!(json_path(&v, "nope").is_none());
    }

    #[test]
    fn exact_placeholder_preserves_type() {
        let vars = |name: &str| -> Option<Value> {
            match name {
                "messages" => Some(json!([{"role":"user"}])),
                "temperature" => Some(json!(0.5)),
                _ => None,
            }
        };
        assert_eq!(render(&json!("{{messages}}"), &vars), json!([{"role":"user"}]));
        assert_eq!(render(&json!("{{temperature}}"), &vars), json!(0.5));
    }

    #[test]
    fn embedded_placeholder_interpolates_string() {
        let vars = |name: &str| -> Option<Value> {
            if name == "model" {
                Some(json!("gpt-4o"))
            } else {
                None
            }
        };
        assert_eq!(render(&json!("model={{model}}!"), &vars), json!("model=gpt-4o!"));
    }

    #[test]
    fn unknown_placeholder_is_preserved() {
        let vars = |_: &str| None;
        assert_eq!(render(&json!("{{nope}}"), &vars), json!("{{nope}}"));
    }

    #[test]
    fn renders_nested_structures() {
        let vars = |name: &str| -> Option<Value> {
            if name == "model" {
                Some(json!("m"))
            } else {
                None
            }
        };
        let t = json!({"model":"{{model}}","meta":{"n":1},"list":[1,2]});
        let r = render(&t, &vars);
        assert_eq!(r["model"], "m");
        assert_eq!(r["meta"]["n"], 1);
        assert_eq!(r["list"], json!([1, 2]));
    }
}
