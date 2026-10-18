//! Step references in batches (`filmcraft-cli run`, MCP `command_batch`).
//!
//! A string parameter that is exactly `$N` or `$N.path` takes the result of step `N` (0-based,
//! counting the steps run so far), and `$prev` / `$prev.path` the result of the step before.
//! `path` is dotted keys and array indexes: `$0.clip`, `$prev.clips.0`, `$2.items.1`. So a graphic
//! made by one step can be styled by the next without a round trip:
//!
//! ```jsonl
//! {"id":"graphics.newText","params":{"text":"300 YIL"}}
//! {"id":"graphics.set","params":{"clip":"$prev.clip","props":{"size":120}}}
//! ```
//!
//! A reference to a step that failed, that does not exist yet, or a path the result lacks is an
//! error naming the reference; other strings (including `$` inside longer text) pass unchanged.

use serde_json::Value;

/// Replace step references inside `params` with values from `results` (`None` = that step failed).
pub fn resolve(params: Value, results: &[Option<Value>]) -> Result<Value, String> {
    match params {
        Value::String(s) => match parse(&s) {
            Some((step, path)) => lookup(&s, step, &path, results),
            None => Ok(Value::String(s)),
        },
        Value::Array(a) => a.into_iter().map(|v| resolve(v, results)).collect::<Result<Vec<_>, _>>().map(Value::Array),
        Value::Object(o) => {
            let mut out = serde_json::Map::with_capacity(o.len());
            for (k, v) in o {
                out.insert(k, resolve(v, results)?);
            }
            Ok(Value::Object(out))
        }
        v => Ok(v),
    }
}

enum Step {
    Index(usize),
    Prev,
}

fn parse(s: &str) -> Option<(Step, Vec<String>)> {
    let rest = s.strip_prefix('$')?;
    let mut parts = rest.split('.');
    let head = parts.next()?;
    let step = if head == "prev" {
        Step::Prev
    } else if !head.is_empty() && head.len() <= 6 && head.bytes().all(|b| b.is_ascii_digit()) {
        Step::Index(head.parse().ok()?)
    } else {
        return None;
    };
    let path: Vec<String> = parts.map(str::to_string).collect();
    if path.iter().any(|p| p.is_empty() || p.len() > 64) || path.len() > 16 {
        return None;
    }
    Some((step, path))
}

fn lookup(raw: &str, step: Step, path: &[String], results: &[Option<Value>]) -> Result<Value, String> {
    let i = match step {
        Step::Index(i) => i,
        Step::Prev => results.len().checked_sub(1).ok_or_else(|| format!("`{raw}`: no step before this one"))?,
    };
    let Some(slot) = results.get(i) else {
        return Err(format!("`{raw}`: step {i} has not run yet ({} so far)", results.len()));
    };
    let Some(mut v) = slot.as_ref() else {
        return Err(format!("`{raw}`: step {i} failed"));
    };
    for key in path {
        let next = match v {
            Value::Object(o) => o.get(key.as_str()),
            Value::Array(a) => key.parse::<usize>().ok().and_then(|n| a.get(n)),
            _ => None,
        };
        v = next.ok_or_else(|| format!("`{raw}`: step {i}'s result has no `{key}`"))?;
    }
    Ok(v.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolves_index_prev_and_paths() {
        let r = vec![Some(json!({"clip": 7, "clips": [3, 4]})), Some(json!({"items": [{"id": 9}]}))];
        let p = json!({"a": "$0.clip", "b": "$prev.items.0.id", "c": ["$0.clips.1"], "d": "price $5", "e": "$0"});
        let out = resolve(p, &r).unwrap();
        assert_eq!(out, json!({"a": 7, "b": 9, "c": [4], "d": "price $5", "e": {"clip": 7, "clips": [3, 4]}}));
    }

    #[test]
    fn errors_name_the_reference() {
        let r = vec![None, Some(json!({"clip": 1}))];
        assert!(resolve(json!("$0.clip"), &r).unwrap_err().contains("failed"));
        assert!(resolve(json!("$5"), &r).unwrap_err().contains("has not run"));
        assert!(resolve(json!("$1.nope"), &r).unwrap_err().contains("no `nope`"));
        assert!(resolve(json!("$prev"), &[]).unwrap_err().contains("no step before"));
    }

    #[test]
    fn plain_strings_pass() {
        for s in ["$", "$x", "$1a", "a$1", "$1..x", "$9999999"] {
            assert_eq!(resolve(json!(s), &[]).unwrap(), json!(s), "{s}");
        }
    }
}
