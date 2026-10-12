//! LLM I/O for extraction: logged calls, flat-object / values-array reshaping and the fact provenance contract.
use super::super::{prompts, schema as formation_schema};
use super::validate::{collapsed, supporting_source, RawFact};
use crate::runtime::{engine::Engine, tenant::Tenant};
use anyhow::{anyhow, Result};
use memory_domain::types::*;
use memory_rules::util;
use memory_store::schema;
use serde_json::{json, Value};
use std::sync::Arc;

/// Persist stage output/final failure; the LLM client durably accounts for every billed reply.
pub async fn call_logged(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    stage: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
) -> Result<Value> {
    call_logged_contract(eng, t, stage, system, user, max_tokens, None).await
}

pub(super) async fn call_logged_contract(
    eng: &Arc<Engine>,
    t: &Arc<Tenant>,
    stage: &str,
    system: &str,
    user: &str,
    max_tokens: u32,
    contract: Option<&crate::runtime::llm::JsonContract>,
) -> Result<Value> {
    let values_only = contract.is_none() && matches!(stage, "P1" | "P1-b");
    let reply = if let Some(contract) = contract {
        eng.llm
            .contract_call_logged(system, user, max_tokens, &t.store, stage, contract)
            .await
    } else if values_only {
        eng.llm
            .values_call_logged(system, user, max_tokens, &t.store, stage)
            .await
    } else {
        eng.llm
            .json_call_logged(system, user, max_tokens, &t.store, stage)
            .await
    };
    let now = util::now_str();
    match reply {
        Ok(r) => {
            let output = if values_only {
                facts_from_values(&r.json)
            } else if contract
                .map(|c| c.name == "facts_object_v2")
                .unwrap_or(false)
            {
                facts_from_flat_object(&r.json)
            } else {
                Ok(r.json.clone())
            };
            let output = match output {
                Ok(output) => output,
                Err(error) => {
                    t.store
                        .log(
                            schema::EXTRACT_LOG,
                            json!({"stage":stage,"createdAt":now,
                        "status":"failed","error":error.to_string(),"modelOutput":r.json}),
                        )
                        .await?;
                    return Err(error);
                }
            };
            let mut record = json!({"stage":stage,"createdAt":now,"variant":prompts::for_tenant(eng, t).variant,
                "systemHash":blake3::hash(system.as_bytes()).to_hex().to_string(),"inputHash":blake3::hash(user.as_bytes()).to_hex().to_string(),
                "outputHash":blake3::hash(output.to_string().as_bytes()).to_hex().to_string(),"output":output,"status":"model_completed",
                "responseLogId":r.response_log_id});
            if values_only || contract.is_some() {
                record["outputFormat"] = json!(if stage == "P2" {
                    formation_schema::EXPERIENCE_FORMAT
                } else if contract.is_some() {
                    formation_schema::FACT_FORMAT
                } else {
                    "fact-values-v1"
                });
                record["modelOutput"] = r.json;
            }
            t.store.log(schema::EXTRACT_LOG, record).await?;
            Ok(output)
        }
        Err(e) => {
            t.store
                .log(
                    schema::EXTRACT_LOG,
                    json!({"stage":stage,"createdAt":now,"status":"failed","error":e.to_string()}),
                )
                .await?;
            Err(e)
        }
    }
}

/// One explicit external contract, no legacy-shape fallback. The model supplies values for flat
/// provenance fields; code owns the internal nested source keys. Raw model replies stay logged.
pub(in crate::algorithms::memos::formation) fn facts_from_flat_object(
    output: &Value,
) -> Result<Value> {
    let items = output["facts"]
        .as_array()
        .ok_or_else(|| anyhow!("facts-object-v2 requires facts array"))?;
    let facts = items
        .iter()
        .map(|item| {
            let mut fields = item
                .as_object()
                .ok_or_else(|| anyhow!("fact must be an object"))?
                .clone();
            let turn = fields
                .remove("sourceTurnId")
                .ok_or_else(|| anyhow!("sourceTurnId missing"))?;
            let span = fields
                .remove("sourceSpan")
                .ok_or_else(|| anyhow!("sourceSpan missing"))?;
            fields.insert("source".into(), json!({"turnId":turn,"span":span}));
            Ok(Value::Object(fields))
        })
        .collect::<Result<Vec<Value>>>()?;
    Ok(json!({"facts":facts}))
}

/// The tuple fixes both arity and types. Missing optional values must retain their slots.
type FactValues = (
    String,
    String,
    Vec<String>,
    String,
    String,
    String,
    String,
    Vec<(String, String)>,
    Option<String>,
);

/// Construct the internal object schema ourselves; preserve rejected row positions for F references.
pub(in crate::algorithms::memos::formation) fn facts_from_values(output: &Value) -> Result<Value> {
    let rows = output
        .as_array()
        .ok_or_else(|| anyhow!("fact-values-v1 requires a top-level array"))?;
    let mut errors = serde_json::Map::new();
    let facts: Vec<Value> = rows.iter().enumerate().map(|(i, row)| {
        match serde_json::from_value::<FactValues>(row.clone()) {
            Ok((text, scope, entities, effective_at, origin, turn_id, span, relations, reason)) => {
                let relations: Vec<Value> = relations.into_iter()
                    .map(|(memory_id, relation)| json!({"memoryId":memory_id,"relation":relation})).collect();
                json!({"text":text,"scope":scope,"entities":entities,"effectiveAt":effective_at,
                    "origin":origin,"source":{"turnId":turn_id,"span":span},
                    "relations":relations,"reason":reason})
            }
            Err(error) => {
                errors.insert((i+1).to_string(), json!(util::truncate_bytes(&error.to_string(),300)));
                Value::Null
            }
        }
    }).collect();
    Ok(json!({"facts":facts,"valueErrors":errors}))
}

/// Provenance is part of the local model contract, so a bad quote receives the same bounded
/// repair exchange as malformed JSON. Never approximate a quote or repair evidence ourselves.
pub(in crate::algorithms::memos::formation) fn fact_source_contract(
    ids: &[String],
    turns: &[TurnRec],
    first_hand: bool,
) -> Value {
    let mut schema = formation_schema::facts_for_turns(ids);
    schema["x-source-turns"] = json!(turns);
    schema["x-both-speakers-first-hand"] = json!(first_hand);
    schema
}

pub(in crate::algorithms::memos::formation) fn validate_fact_source_contract(
    schema: &Value,
    output: &Value,
) -> Result<()> {
    formation_schema::validate_schema(schema, output)?;
    let output = facts_from_flat_object(output)?;
    let turns: Vec<TurnRec> = serde_json::from_value(schema["x-source-turns"].clone())?;
    let first_hand = schema["x-both-speakers-first-hand"]
        .as_bool()
        .unwrap_or(false);
    for (index, value) in output["facts"].as_array().unwrap().iter().enumerate() {
        let fact: RawFact = serde_json::from_value(value.clone())?;
        if !util::is_valid_date(&fact.effective_at) {
            return Err(anyhow!(
                "$.facts[{index}].effectiveAt: invalid calendar date"
            ));
        }
        if fact.text.trim().is_empty() || collapsed(&fact.source.span).is_empty() {
            return Err(anyhow!(
                "$.facts[{index}]: text and source.span must be nonempty"
            ));
        }
        let text = schema["x-source-text"].as_str().map(|text| (text, ""));
        if supporting_source(&fact, &turns, text, first_hand).is_none() {
            return Err(anyhow!("$.facts[{index}].source.span: span_not_verbatim or origin_role_mismatch in turn {}; copy a contiguous excerpt from the permitted source message verbatim", fact.source.turn_id));
        }
    }
    Ok(())
}
