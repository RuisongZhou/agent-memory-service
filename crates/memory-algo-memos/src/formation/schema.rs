//! Local contracts for fact formation and independent session-end experience formation.
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

pub const FACT_FORMAT: &str = "facts-object-v2";
pub const EXPERIENCE_FORMAT: &str = "session-experience-object-v1";

fn object(fields: &[(&str, Value)]) -> Value {
    let properties: Map<String, Value> = fields
        .iter()
        .map(|(k, v)| (k.to_string(), v.clone()))
        .collect();
    json!({"type":"object", "properties":properties,
        "required":fields.iter().map(|(k, _)| *k).collect::<Vec<_>>(), "additionalProperties":false})
}
fn array(items: Value) -> Value {
    json!({"type":"array", "items":items})
}
fn string() -> Value {
    json!({"type":"string"})
}
fn labels(values: &[&str]) -> Value {
    json!({"type":"string", "enum":values})
}
fn relations(values: &[&str]) -> Value {
    array(object(&[
        ("memoryId", string()),
        ("relation", labels(values)),
    ]))
}
fn turn_ids(ids: &[String]) -> Value {
    let mut items = string();
    if !ids.is_empty() {
        items["enum"] = json!(ids);
    }
    json!({"type":"array", "items":items, "maxItems":ids.len()})
}

/// Frequent facts use flat provenance and never request experience/summary fields.
pub fn facts_for_turns(ids: &[String]) -> Value {
    let mut source = string();
    if !ids.is_empty() {
        source["enum"] = json!(ids);
    }
    let fact = object(&[
        ("text", string()),
        ("scope", labels(&["global", "project"])),
        ("entities", array(string())),
        ("effectiveAt", string()),
        ("origin", labels(&["explicit", "user", "assistant"])),
        (
            "relations",
            relations(&["same", "updates", "conflicts", "related"]),
        ),
        ("sourceTurnId", source),
        ("sourceSpan", json!({"type":"string", "maxLength":1024})),
    ]);
    let mut facts = array(fact);
    if ids.is_empty() {
        facts["maxItems"] = json!(0);
    }
    object(&[("facts", facts)])
}

/// Session-end output contains only analysis, experiences and summary.
pub fn experiences_for_turns(ids: &[String]) -> Value {
    let strings = array(string());
    let analysis = object(&[
        ("task", string()),
        (
            "result",
            labels(&["success", "failure", "partial", "unknown"]),
        ),
        ("turnIds", turn_ids(ids)),
    ]);
    let experience = object(&[
        ("situation", string()),
        ("approach", string()),
        ("result", string()),
        ("lesson", string()),
        ("scope", labels(&["global", "project"])),
        ("turnIds", turn_ids(ids)),
        ("relations", relations(&["same", "related"])),
    ]);
    let mut experiences = array(experience);
    experiences["maxItems"] = json!(3);
    object(&[
        ("analysis", array(analysis)),
        ("experiences", experiences),
        (
            "summary",
            object(&[
                ("goal", string()),
                ("outcome", string()),
                ("decisions", strings.clone()),
                ("openIssues", strings),
            ]),
        ),
    ])
}

pub fn validate_schema(schema: &Value, value: &Value) -> Result<()> {
    validate_node(schema, value, "$")
}

fn validate_node(schema: &Value, value: &Value, path: &str) -> Result<()> {
    let matches = |kind: &str| match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "null" => value.is_null(),
        _ => false,
    };
    let valid_type = match &schema["type"] {
        Value::String(kind) => matches(kind),
        Value::Array(kinds) => kinds
            .iter()
            .any(|k| k.as_str().map(matches).unwrap_or(false)),
        _ => false,
    };
    if !valid_type {
        return Err(anyhow!("{path}: expected type {}", schema["type"]));
    }
    if let Some(values) = schema.get("enum").and_then(Value::as_array) {
        if !values.contains(value) {
            return Err(anyhow!("{path}: expected one of {}", schema["enum"]));
        }
    }
    if let Some(obj) = value.as_object() {
        let properties = schema["properties"]
            .as_object()
            .ok_or_else(|| anyhow!("invalid local schema at {path}"))?;
        for key in schema["required"].as_array().unwrap() {
            let key = key.as_str().unwrap();
            if !obj.contains_key(key) {
                return Err(anyhow!("{path}.{key}: required field missing"));
            }
        }
        for (key, field) in obj {
            let child = properties
                .get(key)
                .ok_or_else(|| anyhow!("{path}.{key}: unexpected field"))?;
            validate_node(child, field, &format!("{path}.{key}"))?;
        }
    }
    if let Some(items) = value.as_array() {
        if schema
            .get("maxItems")
            .and_then(Value::as_u64)
            .map(|n| items.len() > n as usize)
            .unwrap_or(false)
        {
            return Err(anyhow!("{path}: exceeds maxItems {}", schema["maxItems"]));
        }
        for (index, item) in items.iter().enumerate() {
            validate_node(&schema["items"], item, &format!("{path}[{index}]"))?;
        }
    }
    if let Some(text) = value.as_str() {
        if schema
            .get("maxLength")
            .and_then(Value::as_u64)
            .map(|n| text.chars().count() > n as usize)
            .unwrap_or(false)
        {
            return Err(anyhow!("{path}: exceeds maxLength {}", schema["maxLength"]));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_experience() -> Value {
        json!({"analysis":[],"experiences":[],"summary":{"goal":"","outcome":"","decisions":[],"openIssues":[]}})
    }
    fn fact() -> Value {
        json!({"facts":[{"text":"A fact","scope":"project","entities":[],
            "effectiveAt":"2026-10-10","origin":"user","sourceTurnId":"T1","sourceSpan":"A fact","relations":[]}]})
    }

    #[test]
    fn fact_and_experience_contracts_are_separate() {
        let schema = facts_for_turns(&["T1".into()]);
        let mut value = fact();
        validate_schema(&schema, &value).unwrap();
        value["summary"] = json!({});
        assert!(validate_schema(&schema, &value).is_err());
        let schema = experiences_for_turns(&["T1".into()]);
        let mut value = empty_experience();
        validate_schema(&schema, &value).unwrap();
        value["facts"] = json!([]);
        assert!(validate_schema(&schema, &value).is_err());
    }

    #[test]
    fn sources_and_limits_are_enforced() {
        let schema = facts_for_turns(&["T1".into()]);
        let mut value = fact();
        value["facts"][0]["sourceTurnId"] = json!("missing");
        assert!(validate_schema(&schema, &value).is_err());
        let mut value = fact();
        value["facts"][0]["sourceSpan"] = json!("证".repeat(1025));
        assert!(validate_schema(&schema, &value)
            .unwrap_err()
            .to_string()
            .contains("maxLength"));
        let schema = experiences_for_turns(&["T1".into()]);
        let mut value = empty_experience();
        let experience = json!({"situation":"deploy","approach":"check","result":"success",
            "lesson":"verify","scope":"project","turnIds":["T1"],"relations":[]});
        value["experiences"] = json!([
            experience.clone(),
            experience.clone(),
            experience.clone(),
            experience
        ]);
        assert!(validate_schema(&schema, &value)
            .unwrap_err()
            .to_string()
            .contains("maxItems"));
    }
}
