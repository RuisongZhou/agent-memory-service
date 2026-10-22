//! Importing a host's own notes into the Mem0 store (docs/design/memory-import.md): one generation call per
//! `extract` unit under a strict JSON contract, with the project's nearest existing facts shown so known
//! content is left out, and history rows for the records import apply writes and retires. Imported records
//! get no content key and no source batch: they are not formation output, so formation never deduplicates
//! into them and the entity projection, which needs inferred provenance, leaves them out. Past session turns
//! are not imported records: formation forms them like captured turns, and `fact_requests` tells import rollback
//! which requests a fact came from. This deviates from the pinned Mem0 algorithm, so it runs only on request and
//! never inside evaluation protocols.
use super::config::Mem0Config;
use super::HISTORY;
use anyhow::{anyhow, ensure, Result};
use memory_domain::import::{ExtractedItem, ExtractedUnit, ImportUnit};
use memory_domain::types::*;
use memory_engine::runtime::{engine::Engine, llm::JsonContract, tenant::Tenant};
use memory_rules::import::units::{UnitLimits, RECORD_OVERHEAD};
use memory_rules::import::validate::{ITEM_KINDS, ITEM_SCOPES};
use memory_rules::util;
use memory_store::schema;
use memory_store::store::Tx;
use serde_json::{json, Value};
use std::sync::Arc;

pub const FORMAT: &str = "mem0-import-v1";
pub const SYSTEM: &str = include_str!("prompts/import.txt");
/// `llm_responses`/`llm_usage`/`extract_log` stage name.
pub const STAGE: &str = "Mem0-import";
/// Most memories one extraction may return; a unit is at most 8 KB, so more would be noise.
pub const MAX_ITEMS: usize = 64;
const FIELDS: [&str; 4] = ["text", "kind", "scope", "evidence"];

/// Most output tokens one extraction may ask for.
const MAX_OUTPUT_TOKENS: u32 = 8192;

/// Output tokens for extracting `note`: the formation limit, or twice the note's estimated tokens when that
/// is more, since every item quotes the note as evidence and a cut-off reply costs repair calls; at most
/// `MAX_OUTPUT_TOKENS` (or the formation limit, if that is higher).
fn output_budget(note: &str, formation_limit: u32) -> u32 {
    let wanted = u32::try_from(util::est_tokens(note).saturating_mul(2)).unwrap_or(u32::MAX);
    wanted.clamp(formation_limit, MAX_OUTPUT_TOKENS.max(formation_limit))
}

/// blake3 hex of the system prompt: the prompt identity on runs and in the extraction cache key.
pub fn prompt_hash() -> String {
    blake3::hash(SYSTEM.as_bytes()).to_hex().to_string()
}

/// Contract check of the shape only: `{"memories": [...]}` and nothing else, each item an object with exactly
/// the four string fields, kind and scope from their enums. Provenance (evidence quotes, names, numbers, size)
/// is checked afterwards by the engine, item by item, and rejects single items instead of asking for a repair.
pub fn validate_output(_schema: &Value, output: &Value) -> Result<()> {
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
        echo_invalid_response: false,
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
pub fn input(
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
pub async fn extract(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
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
        eng.prompt_variant_for(t) == "zh",
    );
    let input_hash = blake3::hash(input.as_bytes()).to_hex().to_string();
    let reply = eng
        .llm
        .contract_call_logged(
            SYSTEM,
            &input,
            output_budget(&unit.text, Mem0Config::from(&eng.cfg).max_tokens),
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

/// The existing facts visible to `project` and `platform` nearest to `text`, by pure vector recall as
/// formation recalls existing facts.
pub async fn similar(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    project: &str,
    platform: &str,
    text: &str,
    limit: usize,
) -> Result<Vec<(MemoryRec, f32)>> {
    super::retrieval::dense(
        eng,
        t,
        text,
        project,
        platform,
        &[T_FACT.into()],
        -1.0,
        limit,
    )
    .await
}

fn history(
    tx: &mut Tx,
    memory_id: &str,
    event: &str,
    run_id: &str,
    text: Option<&str>,
) -> Result<()> {
    let mut row = json!({"memoryId": memory_id, "event": event, "runId": run_id,
        "createdAt": util::now_str()});
    if let Some(text) = text {
        row["text"] = json!(text);
    }
    tx.put(HISTORY, &util::new_id("h"), row)?;
    Ok(())
}

/// History of a record import apply wrote. No content key or source batch: see the module documentation.
pub fn written(rec: &MemoryRec, run_id: &str, tx: &mut Tx) -> Result<()> {
    history(tx, &rec.id, "IMPORT", run_id, Some(&rec.text))
}

/// The input requests formation made the fact `memory_id` from (its source batch); `None` without one, as for
/// a record import wrote.
pub async fn fact_requests(t: &Tenant, memory_id: &str) -> Result<Option<Vec<String>>> {
    let Some(source) = t.store.get_doc(super::SOURCES, memory_id).await? else {
        return Ok(None);
    };
    Ok(Some(serde_json::from_value(
        source["inputRequestIds"].clone(),
    )?))
}

/// History of a record import apply superseded or import rollback retired.
pub fn retired(memory_id: &str, run_id: &str, event: &str, tx: &mut Tx) -> Result<()> {
    history(tx, memory_id, event, run_id, None)
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
        assert_eq!(output_budget("short note", 2000), 2000);
        // 3000 CJK characters are about 3000 tokens: twice that, within the cap.
        assert_eq!(output_budget(&"记".repeat(3000), 2000), 6000);
        assert_eq!(output_budget(&"记".repeat(8000), 2000), MAX_OUTPUT_TOKENS);
        // A formation limit above the cap is kept.
        assert_eq!(output_budget("short note", 10000), 10000);
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
