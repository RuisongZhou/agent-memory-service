//! Durable receipt inspection shared by both memory algorithms.
use crate::runtime::{engine::Engine, tenant::Tenant};
use memory_domain::types::*;
use memory_store::schema::{self, request_kind};
use memory_store::store::{self, from_doc};
use serde_json::{json, Value};
use std::sync::Arc;

pub async fn get_task(_eng: &Arc<Engine>, t: &Arc<Tenant>, req: GetTaskReq) -> ApiResult<Value> {
    store::safe_id(&req.request_id)
        .map_err(|_| ApiError::InvalidArgument("invalid requestId".into()))?;
    let doc = t
        .store
        .get_doc(schema::REQUESTS, &req.request_id)
        .await?
        .ok_or_else(|| ApiError::NotFound("request not found".into()))?;
    let r: RequestRec = from_doc(doc.clone())?;
    let mut out = json!({"requestId":r.request_id,"status":r.status,"memoryIds":r.memory_ids,"error":r.error});
    if doc["kind"] == request_kind::MEM0 {
        out["inputSaved"] = json!(true);
        out["formed"] = doc["formed"].clone();
        out["searchable"] = doc["searchable"].clone();
        out["latencyMs"] = doc["latencyMs"].clone();
    }
    Ok(out)
}
