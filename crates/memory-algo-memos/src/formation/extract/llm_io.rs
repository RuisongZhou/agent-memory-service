//! LLM I/O for extraction: logged JSON-object calls, flat provenance and source validation.
use super::super::{prompts, schema as formation_schema};
use super::validate::{collapsed, supporting_source, RawFact};
use anyhow::{anyhow, Result};
use memory_domain::types::*;
use memory_engine::runtime::{engine::Engine, tenant::Tenant};
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
    contract: Option<&memory_engine::runtime::llm::JsonContract>,
) -> Result<Value> {
    let reply = if let Some(contract) = contract {
        eng.llm
            .contract_call_logged(system, user, max_tokens, &t.store, stage, contract)
            .await
    } else {
        eng.llm
            .json_call_logged(system, user, max_tokens, &t.store, stage)
            .await
    };
    let now = util::now_str();
    match reply {
        Ok(r) => {
            let output = if contract
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
            if contract.is_some() {
                record["outputFormat"] = json!(if stage == "P2" {
                    formation_schema::EXPERIENCE_FORMAT
                } else {
                    formation_schema::FACT_FORMAT
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
pub(in crate::formation) fn facts_from_flat_object(output: &Value) -> Result<Value> {
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

/// Provenance is part of the local model contract, so a bad quote receives the same bounded
/// repair exchange as malformed JSON. Never approximate a quote or repair evidence ourselves.
pub(in crate::formation) fn fact_source_contract(
    ids: &[String],
    turns: &[TurnRec],
    first_hand: bool,
) -> Value {
    let mut schema = formation_schema::facts_for_turns(ids);
    schema["x-source-turns"] = json!(turns);
    schema["x-both-speakers-first-hand"] = json!(first_hand);
    schema
}

pub(in crate::formation) fn validate_fact_source_contract(
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
