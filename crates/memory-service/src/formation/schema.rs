//! The provider and the local boundary use the same small, explicit schema contract.
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};

pub const FORMAT: &str = "formation-object-v2";
pub const FACT_FORMAT: &str = "facts-object-v1";
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

pub fn schema() -> Value {
    let strings = array(string());
    let relation = |values: &[&str]| {
        array(object(&[
            ("memoryId", string()),
            ("relation", labels(values)),
        ]))
    };
    let fact = object(&[
        ("text", string()),
        ("scope", labels(&["global", "project"])),
        ("entities", strings.clone()),
        ("effectiveAt", string()),
        ("origin", labels(&["explicit", "user", "assistant"])),
        (
            "source",
            object(&[("turnId", string()), ("span", string())]),
        ),
        (
            "relations",
            relation(&["same", "updates", "conflicts", "related"]),
        ),
        ("reason", json!({"type":["string","null"]})),
    ]);
    let analysis = object(&[
        ("task", string()),
        (
            "result",
            labels(&["success", "failure", "partial", "unknown"]),
        ),
        ("turnIds", strings.clone()),
    ]);
    let experience = object(&[
        ("situation", string()),
        ("approach", string()),
        ("result", string()),
        ("lesson", string()),
        ("scope", labels(&["global", "project"])),
        ("turnIds", strings.clone()),
        ("relations", relation(&["same", "related"])),
    ]);
    object(&[
        ("facts", array(fact)),
        ("analysis", array(analysis)),
        ("experiences", array(experience)),
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

/// Validate precisely the type/enum/required/additionalProperties/items subset emitted above.
/// Content, provenance and experience admission remain in the formation rules.
pub fn validate(value: &Value) -> Result<()> {
    validate_schema(&schema(), value)
}

/// Constrain source labels to actual supplied turns, rather than arbitrary M/F memory labels.
/// The same constraints already apply at the provenance gate; this also prevents decode loops.
pub fn for_turns(fact_turn_ids: &[String], context_turn_ids: &[String]) -> Value {
    let mut schema = schema();
    if fact_turn_ids.is_empty() {
        schema["properties"]["facts"]["maxItems"] = json!(0);
    } else {
        schema["properties"]["facts"]["items"]["properties"]["source"]["properties"]["turnId"]
            ["enum"] = json!(fact_turn_ids);
    }
    schema["properties"]["facts"]["items"]["properties"]["source"]["properties"]["span"]
        ["maxLength"] = json!(1024);
    for section in ["analysis", "experiences"] {
        let ids = &mut schema["properties"][section]["items"]["properties"]["turnIds"];
        ids["maxItems"] = json!(context_turn_ids.len());
        if !context_turn_ids.is_empty() {
            ids["items"]["enum"] = json!(context_turn_ids);
        }
    }
    schema["properties"]["experiences"]["maxItems"] = json!(3);
    schema
}

/// Contracts stay local; the provider receives json_object rather than json_schema.
pub fn facts_for_turns(turn_ids: &[String]) -> Value {
    let full = for_turns(turn_ids, turn_ids);
    let mut facts = full["properties"]["facts"].clone();
    facts["items"]["properties"]
        .as_object_mut()
        .unwrap()
        .remove("reason");
    facts["items"]["required"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "reason");
    object(&[("facts", facts)])
}

pub fn experiences_for_turns(turn_ids: &[String]) -> Value {
    let full = for_turns(&[], turn_ids);
    object(&[
        ("analysis", full["properties"]["analysis"].clone()),
        ("experiences", full["properties"]["experiences"].clone()),
        ("summary", full["properties"]["summary"].clone()),
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

    #[test]
    fn separate_contracts_reject_cross_stage_fields_and_invalid_sources() {
        let fact_schema = facts_for_turns(&["T1".into()]);
        let mut fact = json!({"facts":[{"text":"A fact","scope":"project","entities":[],
            "effectiveAt":"2026-10-10","origin":"user","source":{"turnId":"T1","span":"A fact"},"relations":[]}]});
        assert!(validate_schema(&fact_schema, &fact).is_ok());
        fact["summary"] = json!({});
        assert!(validate_schema(&fact_schema, &fact).is_err());
        fact.as_object_mut().unwrap().remove("summary");
        fact["facts"][0]["source"]["turnId"] = json!("missing");
        assert!(validate_schema(&fact_schema, &fact).is_err());
        let exp_schema = experiences_for_turns(&["T1".into()]);
        let mut exp = json!({"analysis":[],"experiences":[],"summary":{"goal":"","outcome":"","decisions":[],"openIssues":[]}});
        assert!(validate_schema(&exp_schema, &exp).is_ok());
        exp["facts"] = json!([]);
        assert!(validate_schema(&exp_schema, &exp).is_err());
    }
    fn empty() -> Value {
        json!({"facts":[],"analysis":[],"experiences":[],"summary":{"goal":"","outcome":"","decisions":[],"openIssues":[]}})
    }
    #[test]
    fn rejects_old_flat_arrays_missing_sections_and_wrong_nested_types() {
        validate(&empty()).unwrap();
        assert!(validate(&json!([[], [], [], ["", "", [], []]])).is_err());
        let mut v = empty();
        v.as_object_mut().unwrap().remove("summary");
        assert!(validate(&v).unwrap_err().to_string().contains("$.summary"));
        let mut v = empty();
        v["analysis"] = json!([{"task":"deploy","result":"unknown","turnIds":"T1"}]);
        assert!(validate(&v)
            .unwrap_err()
            .to_string()
            .contains("$.analysis[0].turnIds"));
        v["analysis"][0]["turnIds"] = json!(["T1"]);
        v["analysis"][0]["result"] = json!("guessed");
        assert!(validate(&v).is_err());
        let mut v = empty();
        v["unexpected"] = json!([]);
        assert!(validate(&v).is_err());
    }
    #[test]
    fn nullable_reason_and_canonical_fact_source_are_checked() {
        let mut v = empty();
        v["facts"] = json!([{"text":"A fact","scope":"project","entities":[],"effectiveAt":"2023-05-08",
            "origin":"user","source":{"turnId":"T1","span":"A fact"},"relations":[],"reason":null}]);
        validate(&v).unwrap();
        v["facts"][0]["source"]["span"] = json!(["wrong"]);
        assert!(validate(&v).is_err());
    }
    #[test]
    fn rejects_memory_labels_and_unbounded_repeated_turn_lists() {
        let schema = for_turns(&["T2".into()], &["T1".into(), "T2".into()]);
        let mut value = empty();
        value["analysis"] = json!([{"task":"deploy","result":"unknown","turnIds":["F1"]}]);
        assert!(validate_schema(&schema, &value).is_err());
        value["analysis"][0]["turnIds"] = json!(["T1", "T2"]);
        validate_schema(&schema, &value).unwrap();
        value["analysis"][0]["turnIds"] = json!(["T1", "T1", "T1"]);
        assert!(validate_schema(&schema, &value)
            .unwrap_err()
            .to_string()
            .contains("maxItems"));
        validate_schema(&for_turns(&[], &[]), &empty()).unwrap();
    }
}
