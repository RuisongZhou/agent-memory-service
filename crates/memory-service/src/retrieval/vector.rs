//! Shared ANN access. Filtering and ranking remain in the selected algorithm.
use crate::runtime::tenant::Tenant;
use anyhow::Result;
use serde_json::json;

pub(crate) async fn hnsw_search(t: &Tenant, q: &[f32], k: usize) -> Result<Vec<(String, f32)>> {
    let ef = (k * 2).max(100);
    let sql = format!(
        "SELECT mid, vector::similarity::cosine(embedding, $vec) AS score FROM memories \
         WHERE embedding <|{k},{ef}|> $vec ORDER BY score DESC"
    );
    let rows = t.store.select(&sql, vec![("vec", json!(q))]).await?;
    Ok(rows
        .into_iter()
        .filter_map(|r| {
            let id = r.get("mid")?.as_str()?.to_string();
            let s = r.get("score").and_then(|v| v.as_f64()).unwrap_or(0.0) as f32;
            Some((id, s))
        })
        .collect())
}
