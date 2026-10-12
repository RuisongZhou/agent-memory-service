//! Durable receipt inspection shared by both memory algorithms.
use crate::domain::types::*;
use crate::runtime::{engine::Engine, tenant::Tenant};
use crate::storage::store::{self, from_doc};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn get_task(_eng: &Arc<Engine>, t: &Arc<Tenant>, req: GetTaskReq) -> ApiResult<Value> {
    store::safe_id(&req.request_id)
        .map_err(|_| ApiError::InvalidArgument("invalid requestId".into()))?;
    let doc = t
        .store
        .get_doc("requests", &req.request_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("request not found".into()))?;
    let r: RequestRec = from_doc(doc.clone())?;
    let mut out = json!({"requestId":r.request_id,"status":r.status,"memoryIds":r.memory_ids,"error":r.error});
    if doc["kind"] == "mem0" {
        out["inputSaved"] = json!(true);
        out["formed"] = doc["formed"].clone();
        out["searchable"] = doc["searchable"].clone();
        out["latencyMs"] = doc["latencyMs"].clone();
    }
    Ok(out)
}
