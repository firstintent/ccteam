//! Turning a worker's prose into a checked JSON value.
//!
//! `agent(task, {schema})` promises the script an object, not a paragraph.
//! ccteam cannot use Claude Code's trick of forcing a `StructuredOutput` tool
//! call, because a hire may be any harness — codex, grok, kimi, dsh — and
//! there is no cross-vendor structured-output channel. So extraction is
//! **deterministic text surgery**, never a model call (engine-zero-LLM is a
//! red line), and the recovery path is a follow-up turn in the same session.
//!
//! Extraction ladder, first hit wins:
//!   1. the whole reply parses as JSON;
//!   2. the first fenced code block parses as JSON;
//!   3. the first balanced `{...}` or `[...]` span parses as JSON.
//!
//! Validation is a deliberately small JSON-Schema subset — enough to catch a
//! worker that answered with the wrong shape, without pulling a full
//! draft-2020 validator into the dependency graph. Unknown keywords are
//! IGNORED rather than rejected, so a schema written for a fuller validator
//! still works here; it is simply checked less strictly. That is the honest
//! trade: this layer exists to trigger a retry, not to be a spec oracle.

use serde_json::Value;

/// The default follow-up sent when a reply does not match the schema.
///
/// Fixed text, not a generated critique: the runner must not reason about the
/// worker's answer (that would be an LLM in the engine). It states the
/// mechanical fact and what to send instead.
pub const SCHEMA_RETRY_PROMPT: &str =
    "Your reply did not match the requested JSON schema. Reply with ONLY the JSON value \
     itself — no prose, no explanation, no code fence.";

/// Pull the first plausible JSON value out of a worker reply.
pub fn extract_json(text: &str) -> Option<Value> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        return Some(v);
    }
    if let Some(v) = fenced_block(trimmed).and_then(|b| serde_json::from_str::<Value>(&b).ok()) {
        return Some(v);
    }
    balanced_span(trimmed).and_then(|s| serde_json::from_str::<Value>(&s).ok())
}

/// Body of the first ``` fenced block, language tag (```json) discarded.
fn fenced_block(text: &str) -> Option<String> {
    let open = text.find("```")?;
    let after = &text[open + 3..];
    // Drop the info string up to the first newline.
    let body_start = after.find('\n')? + 1;
    let body = &after[body_start..];
    let close = body.find("```")?;
    Some(body[..close].trim().to_string())
}

/// First balanced `{...}` / `[...]` span, string-aware so a brace inside a
/// quoted string cannot close the span early.
fn balanced_span(text: &str) -> Option<String> {
    let bytes: Vec<(usize, char)> = text.char_indices().collect();
    let start_idx = bytes.iter().position(|(_, c)| *c == '{' || *c == '[')?;
    let open = bytes[start_idx].1;
    let close = if open == '{' { '}' } else { ']' };
    let mut depth = 0usize;
    let mut in_str = false;
    let mut escaped = false;
    for &(off, c) in &bytes[start_idx..] {
        if in_str {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => in_str = true,
            _ if c == open => depth += 1,
            _ if c == close => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[bytes[start_idx].0..off + c.len_utf8()].to_string());
                }
            }
            _ => {}
        }
    }
    None
}

/// Check `value` against `schema`. `Err` carries a one-line reason suitable
/// for a run report.
pub fn validate(schema: &Value, value: &Value) -> Result<(), String> {
    check(schema, value, "$")
}

fn check(schema: &Value, value: &Value, path: &str) -> Result<(), String> {
    let Some(obj) = schema.as_object() else {
        // A non-object schema (e.g. `true`) accepts anything.
        return Ok(());
    };

    if let Some(types) = obj.get("type") {
        let ok = match types {
            Value::String(t) => type_matches(t, value),
            Value::Array(list) => list
                .iter()
                .filter_map(|t| t.as_str())
                .any(|t| type_matches(t, value)),
            _ => true,
        };
        if !ok {
            return Err(format!(
                "{path}: expected type {}, got {}",
                types,
                type_name(value)
            ));
        }
    }

    if let Some(Value::Array(allowed)) = obj.get("enum") {
        if !allowed.contains(value) {
            return Err(format!(
                "{path}: value is not one of the allowed enum values"
            ));
        }
    }
    if let Some(expected) = obj.get("const") {
        if expected != value {
            return Err(format!("{path}: value does not equal the required const"));
        }
    }

    if let Some(fields) = obj.get("required").and_then(Value::as_array) {
        let map = value.as_object();
        for f in fields.iter().filter_map(Value::as_str) {
            let present = map.is_some_and(|m| m.contains_key(f));
            if !present {
                return Err(format!("{path}: missing required property `{f}`"));
            }
        }
    }

    if let (Some(props), Some(map)) = (
        obj.get("properties").and_then(Value::as_object),
        value.as_object(),
    ) {
        for (k, sub) in props {
            if let Some(v) = map.get(k) {
                check(sub, v, &format!("{path}.{k}"))?;
            }
        }
        if obj.get("additionalProperties") == Some(&Value::Bool(false)) {
            for k in map.keys() {
                if !props.contains_key(k) {
                    return Err(format!("{path}: unexpected property `{k}`"));
                }
            }
        }
    }

    if let (Some(items), Some(list)) = (obj.get("items"), value.as_array()) {
        for (i, v) in list.iter().enumerate() {
            check(items, v, &format!("{path}[{i}]"))?;
        }
    }
    if let Some(list) = value.as_array() {
        if let Some(min) = obj.get("minItems").and_then(Value::as_u64) {
            if (list.len() as u64) < min {
                return Err(format!("{path}: expected at least {min} items"));
            }
        }
        if let Some(max) = obj.get("maxItems").and_then(Value::as_u64) {
            if (list.len() as u64) > max {
                return Err(format!("{path}: expected at most {max} items"));
            }
        }
    }

    if let Some(n) = value.as_f64() {
        if let Some(min) = obj.get("minimum").and_then(Value::as_f64) {
            if n < min {
                return Err(format!("{path}: {n} is below minimum {min}"));
            }
        }
        if let Some(max) = obj.get("maximum").and_then(Value::as_f64) {
            if n > max {
                return Err(format!("{path}: {n} is above maximum {max}"));
            }
        }
    }

    for (kw, combine) in [
        ("allOf", Combine::All),
        ("anyOf", Combine::Any),
        ("oneOf", Combine::One),
    ] {
        if let Some(list) = obj.get(kw).and_then(Value::as_array) {
            let hits = list
                .iter()
                .filter(|s| check(s, value, path).is_ok())
                .count();
            let ok = match combine {
                Combine::All => hits == list.len(),
                Combine::Any => hits >= 1,
                Combine::One => hits == 1,
            };
            if !ok {
                return Err(format!("{path}: does not satisfy `{kw}`"));
            }
        }
    }

    Ok(())
}

enum Combine {
    All,
    Any,
    One,
}

fn type_matches(t: &str, v: &Value) -> bool {
    match t {
        "object" => v.is_object(),
        "array" => v.is_array(),
        "string" => v.is_string(),
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        // Unknown type keyword: not our business to reject.
        _ => true,
    }
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn whole_reply_that_is_json_wins() {
        let v = extract_json("  {\"a\": 1}  ").expect("json");
        assert_eq!(v, json!({"a": 1}));
    }

    #[test]
    fn fenced_block_is_extracted() {
        let v = extract_json("Here you go:\n```json\n{\"ok\": true}\n```\nHope that helps.")
            .expect("fenced json");
        assert_eq!(v, json!({"ok": true}));
    }

    #[test]
    fn balanced_span_survives_surrounding_prose() {
        let v = extract_json("I found {\"bugs\": [{\"file\": \"a.rs\"}]} in the tree.")
            .expect("balanced span");
        assert_eq!(v, json!({"bugs": [{"file": "a.rs"}]}));
    }

    #[test]
    fn braces_inside_strings_do_not_close_the_span() {
        let v = extract_json(r#"note: {"msg": "a } here", "n": 1} done"#).expect("span");
        assert_eq!(v, json!({"msg": "a } here", "n": 1}));
    }

    #[test]
    fn arrays_are_extracted_too() {
        assert_eq!(extract_json("result: [1, 2, 3]"), Some(json!([1, 2, 3])));
    }

    #[test]
    fn prose_without_json_yields_nothing() {
        assert_eq!(extract_json("I could not find anything."), None);
        assert_eq!(extract_json("   "), None);
    }

    #[test]
    fn type_and_required_are_enforced() {
        let schema = json!({
            "type": "object",
            "required": ["title", "count"],
            "properties": { "title": {"type": "string"}, "count": {"type": "integer"} }
        });
        validate(&schema, &json!({"title": "x", "count": 2})).expect("valid");
        let err = validate(&schema, &json!({"title": "x"})).expect_err("missing count");
        assert!(err.contains("count"), "{err}");
        let err = validate(&schema, &json!({"title": 1, "count": 2})).expect_err("wrong type");
        assert!(err.contains("expected type"), "{err}");
    }

    #[test]
    fn nested_arrays_enums_and_extra_properties_are_checked() {
        let schema = json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "items": {
                    "type": "array",
                    "minItems": 1,
                    "items": {"type": "object", "properties": {"kind": {"enum": ["a", "b"]}}}
                }
            }
        });
        validate(&schema, &json!({"items": [{"kind": "a"}]})).expect("valid");
        assert!(
            validate(&schema, &json!({"items": []})).is_err(),
            "minItems"
        );
        assert!(
            validate(&schema, &json!({"items": [{"kind": "z"}]})).is_err(),
            "enum"
        );
        let err = validate(&schema, &json!({"items": [], "extra": 1})).expect_err("extra");
        assert!(err.contains("items") || err.contains("extra"), "{err}");
    }

    #[test]
    fn unknown_keywords_are_ignored_rather_than_rejected() {
        // Schemas are authored for fuller validators; an unknown keyword must
        // not turn a good reply into a retry loop.
        let schema = json!({"type": "object", "$comment": "hi", "unevaluatedProperties": false});
        validate(&schema, &json!({"anything": 1})).expect("unknown keywords ignored");
    }
}
