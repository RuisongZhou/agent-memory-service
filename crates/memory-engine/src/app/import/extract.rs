//! Extraction of an `extract` unit (docs/design/memory-import.md §6.6): one generation call under a strict JSON
//! contract, with the project's nearest existing facts shown so known content is left out. The prompt
//! (`extract.txt`) is original to this service and belongs to no memory algorithm: its items are checked by the pure
//! rules of `memory_rules::import::validate` and `apply` writes them as plain records, so extraction needs nothing of
//! the algorithm but the existing facts it recalls (`MemoryAlgorithm::nearest_facts`).
use anyhow::{anyhow, ensure, Result};
use serde_json::{json, Value};
use std::sync::Arc;

use crate::runtime::{engine::Engine, instance::Instance, llm::JsonContract};
use memory_domain::import::{ExtractedItem, ExtractedUnit, ImportUnit};
use memory_domain::types::*;
use memory_rules::import::units::{UnitLimits, RECORD_OVERHEAD};
use memory_rules::import::validate::{ITEM_KINDS, ITEM_SCOPES};
use memory_rules::util;
use memory_store::schema;

/// The prompt identity recorded on runs as `promptFormat`.
pub const FORMAT: &str = "import-v1";
pub const SYSTEM: &str = include_str!("extract.txt");
/// `llm_responses`/`llm_usage`/`extract_log` stage name.
pub const STAGE: &str = "import";
/// Most memories one extraction may return; a unit is at most 8 KB, so more would be noise.
pub const MAX_ITEMS: usize = 64;
const FIELDS: [&str; 4] = ["text", "kind", "scope", "evidence"];

/// Fewest and most output tokens one extraction asks for.
const MIN_OUTPUT_TOKENS: u32 = 2000;
const MAX_OUTPUT_TOKENS: u32 = 8192;

/// Output tokens for extracting `note`: twice the note's estimated tokens, since every item quotes the note as
/// evidence and a cut-off reply costs repair calls; at least `MIN_OUTPUT_TOKENS` and at most `MAX_OUTPUT_TOKENS`.
fn output_budget(note: &str) -> u32 {
    let wanted = u32::try_from(util::est_tokens(note).saturating_mul(2)).unwrap_or(u32::MAX);
    wanted.clamp(MIN_OUTPUT_TOKENS, MAX_OUTPUT_TOKENS)
}

/// blake3 hex of the system prompt: the prompt identity on runs and in the extraction cache key.
pub fn prompt_hash() -> String {
    util::hex_hash(SYSTEM)
}

/// Contract check of the shape only: `{"memories": [...]}` and nothing else, each item an object with exactly
/// the four string fields, kind and scope from their enums. Provenance (evidence quotes, names, numbers, size)
/// is checked afterwards by `propose`, item by item, and rejects single items instead of asking for a repair.
fn validate_output(_schema: &Value, output: &Value) -> Result<()> {
    let object = output
        .as_object()
        .ok_or_else(|| anyhow!("the response must be a JSON object"))?;
    if let Some(key) = object.keys().find(|k| *k != "memories") {
        return Err(anyhow!(
            "unexpected top-level field {key:?}; only \"memories\" is allowed"
        ));
    }
    let items = object
        .get("memories")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("\"memories\" must be an array"))?;
    ensure!(
        items.len() <= MAX_ITEMS,
        "memories has more than {MAX_ITEMS} items; keep only durable memories"
    );
    for (i, item) in items.iter().enumerate() {
        let fields = item
            .as_object()
            .ok_or_else(|| anyhow!("memories[{i}] must be an object"))?;
        if let Some(key) = fields.keys().find(|k| !FIELDS.contains(&k.as_str())) {
            return Err(anyhow!(
                "memories[{i}] has unexpected field {key:?}; allowed fields are {FIELDS:?}"
            ));
        }
        for key in FIELDS {
            ensure!(
                fields.get(key).is_some_and(Value::is_string),
                "memories[{i}].{key} must be a JSON string"
            );
        }
        let kind = item["kind"].as_str().unwrap_or_default();
        ensure!(
            ITEM_KINDS.contains(&kind),
            "memories[{i}].kind must be one of {ITEM_KINDS:?}, not {kind:?}"
        );
        let scope = item["scope"].as_str().unwrap_or_default();
        ensure!(
            ITEM_SCOPES.contains(&scope),
            "memories[{i}].scope must be one of {ITEM_SCOPES:?}, not {scope:?}"
        );
    }
    Ok(())
}

fn contract() -> JsonContract {
    JsonContract {
        name: FORMAT,
        validate: validate_output,
        schema: json!({
        "type":"object","required":["memories"],"additionalProperties":false,
        "properties":{"memories":{"type":"array","maxItems":MAX_ITEMS,"items":{
            "type":"object","required":FIELDS,"additionalProperties":false,
            "properties":{"text":{"type":"string","minLength":1},"kind":{"enum":ITEM_KINDS},
                "scope":{"enum":ITEM_SCOPES},"evidence":{"type":"string","minLength":1}}}}}}),
    }
}

/// The user message of one extraction: the unit's source metadata, the existing memories, the length the
/// unit limits allow and the note itself. Everything variable is here; the system prompt is static.
fn input(
    unit: &ImportUnit,
    project: &str,
    limits: &UnitLimits,
    existing: &[MemoryRec],
    zh: bool,
) -> String {
    let source = json!({"host": unit.host, "project": project, "headingPath": unit.heading_path,
        "statedAt": unit.stated_at, "kindHint": unit.kind_hint});
    let texts: Vec<&str> = existing.iter().map(|m| m.text.as_str()).collect();
    let cap = limits.hard_cap().saturating_sub(RECORD_OVERHEAD);
    let target = limits.target().saturating_sub(RECORD_OVERHEAD);
    let mut sections = vec![
        format!("## Source\n{source}"),
        format!("## Existing Memories\n{}", json!(texts)),
        format!(
            "## Length\nEach memory: about {target} and at most {cap} Chinese characters, or four times as many characters of other text."
        ),
        format!("## Note\n<note>\n{}\n</note>", unit.text),
    ];
    if zh {
        sections.push(
            "## Language Requirement\nWrite each memory in the same language and script as the note."
                .into(),
        );
    }
    sections.push("# Output:".into());
    sections.join("\n\n")
}

/// One generation call for `unit`. The raw provider response is persisted before parsing, and an
/// `extract_log` row records the parsed output with the prompt hashes. Items are returned unvalidated.
pub(super) async fn extract(
    eng: &Arc<Engine>,
    t: &Arc<Instance>,
    project: &str,
    unit: &ImportUnit,
    limits: &UnitLimits,
    existing: &[MemoryRec],
) -> Result<ExtractedUnit> {
    let input = input(
        unit,
        project,
        limits,
        existing,
        eng.cfg.prompt_variant == "zh",
    );
    let input_hash = util::hex_hash(&input);
    let reply = eng
        .llm
        .contract_call_logged(
            SYSTEM,
            &input,
            output_budget(&unit.text),
            &t.store,
            STAGE,
            &contract(),
        )
        .await?;
    let prompt_hash = prompt_hash();
    t.store
        .log(
            schema::EXTRACT_LOG,
            json!({"stage":STAGE,"outputFormat":FORMAT,"project":project,"unit":unit.unit,
            "status":"model_completed","output":reply.json,"responseLogId":reply.response_log_id,
            "systemHash":prompt_hash,"inputHash":input_hash}),
        )
        .await?;
    let items: Vec<ExtractedItem> = serde_json::from_value(reply.json["memories"].clone())?;
    Ok(ExtractedUnit {
        items,
        response_log_id: reply.response_log_id,
        input_hash,
        prompt_format: FORMAT.into(),
        prompt_hash,
        model: eng.llm.model.clone(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit() -> ImportUnit {
        ImportUnit {
            unit: "codex:thread-1#task-1".into(),
            host: "codex".into(),
            mode: "extract".into(),
            text: "- The release job runs make deploy.\n- Alice owns payments.".into(),
            stated_at: "2026-05-03T09:00:00Z".into(),
            heading_path: vec!["Release".into(), "Task 1".into()],
            ..Default::default()
        }
    }

    #[test]
    fn output_budget_grows_with_the_note() {
        assert_eq!(output_budget("short note"), MIN_OUTPUT_TOKENS);
        // 3000 CJK characters are about 3000 tokens: twice that, within the cap.
        assert_eq!(output_budget(&"记".repeat(3000)), 6000);
        assert_eq!(output_budget(&"记".repeat(8000)), MAX_OUTPUT_TOKENS);
    }

    #[test]
    fn contract_checks_shape_and_enums_only() {
        let schema = contract().schema;
        let item = json!({"text":"x","kind":"fact","scope":"project","evidence":"y"});
        assert!(validate_output(&schema, &json!({ "memories": [item] })).is_ok());
        assert!(validate_output(&schema, &json!({"memories": []})).is_ok());
        // Provenance is the engine's: an evidence that quotes nothing still has the right shape.
        let unquoted =
            json!({"text":"x","kind":"fact","scope":"project","evidence":"not in the note"});
        assert!(validate_output(&schema, &json!({ "memories": [unquoted] })).is_ok());
        let fails = |output: Value, needle: &str| {
            let error = validate_output(&schema, &output).unwrap_err().to_string();
            assert!(error.contains(needle), "{error}");
        };
        fails(json!({"items": []}), "unexpected top-level field");
        fails(json!({"memories": {}}), "must be an array");
        fails(json!({ "memories": ["x"] }), "must be an object");
        fails(
            json!({"memories":[{"text":"x","kind":"fact","scope":"project"}]}),
            "evidence must be a JSON string",
        );
        fails(
            json!({"memories":[{"text":"x","kind":"fact","scope":"project","evidence":"y","why":"z"}]}),
            "unexpected field \"why\"",
        );
        fails(
            json!({"memories":[{"text":"x","kind":"transient","scope":"project","evidence":"y"}]}),
            "kind must be one of",
        );
        fails(
            json!({"memories":[{"text":"x","kind":"fact","scope":"","evidence":"y"}]}),
            "scope must be one of",
        );
        fails(
            json!({"memories":[{"text":7,"kind":"fact","scope":"project","evidence":"y"}]}),
            "text must be a JSON string",
        );
        let many = vec![item; MAX_ITEMS + 1];
        fails(json!({ "memories": many }), "more than 64 items");
        assert_eq!(
            schema["properties"]["memories"]["items"]["properties"]["kind"]["enum"],
            json!(ITEM_KINDS)
        );
        assert_eq!(
            schema["properties"]["memories"]["items"]["properties"]["scope"]["enum"],
            json!(ITEM_SCOPES)
        );
    }

    #[test]
    fn input_carries_source_existing_length_and_note() {
        let existing = vec![MemoryRec {
            text: "The release job is manual.".into(),
            ..Default::default()
        }];
        let input = input(&unit(), "proj", &UnitLimits::default(), &existing, false);
        let sections: Vec<&str> = input.split("\n\n").collect();
        assert!(sections[0].starts_with("## Source\n"));
        let source: Value = serde_json::from_str(&sections[0]["## Source\n".len()..]).unwrap();
        assert_eq!(
            source,
            json!({"host":"codex","project":"proj","headingPath":["Release","Task 1"],
                "statedAt":"2026-05-03T09:00:00Z","kindHint":null})
        );
        assert_eq!(
            sections[1],
            "## Existing Memories\n[\"The release job is manual.\"]"
        );
        assert_eq!(
            sections[2],
            "## Length\nEach memory: about 72 and at most 192 Chinese characters, or four times as many characters of other text."
        );
        assert!(input.contains(
            "## Note\n<note>\n- The release job runs make deploy.\n- Alice owns payments.\n</note>"
        ));
        assert!(input.ends_with("\n\n# Output:"));
        assert!(!input.contains("Language Requirement"));
        let zh = super::input(&unit(), "proj", &UnitLimits::default(), &[], true);
        assert!(zh.contains("## Existing Memories\n[]"));
        assert!(zh.ends_with(
            "## Language Requirement\nWrite each memory in the same language and script as the note.\n\n# Output:"
        ));
    }

    #[test]
    fn prompt_is_the_memory_importer_and_names_every_kind() {
        assert_eq!(SYSTEM.lines().next(), Some("You are a Memory Importer."));
        for kind in ITEM_KINDS.iter().chain(ITEM_SCOPES.iter()) {
            assert!(SYSTEM.contains(&format!("\"{kind}\"")), "{kind}");
        }
        assert!(SYSTEM.contains("{\"memories\": []}"));
        assert_eq!(prompt_hash().len(), 64);
    }
}
