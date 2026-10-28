//! `platform_activity`: the platforms that called the daemon in the last 30 days, and through which of their
//! programs (`X-Memory-Client`). Calls show what is connected and works, which configuration on disk cannot:
//! a platform must also trust the hooks and the MCP server it was given.
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::sync::Arc;

use crate::runtime::instance::Instance;
use memory_domain::types::*;
use memory_rules::util;

const WINDOW_DAYS: i64 = 30;

/// Calls counted together: how many, and the latest.
#[derive(Default)]
struct Seen {
    count: i64,
    last_at: String,
}

impl Seen {
    fn add(&mut self, count: i64, at: &str) {
        self.count += count;
        if at > self.last_at.as_str() {
            self.last_at = at.to_string();
        }
    }
}

/// One platform's calls, in all and by channel.
#[derive(Default)]
struct Platform {
    all: Seen,
    channels: BTreeMap<&'static str, Seen>,
}

/// What a call shows working: dialogue the hooks captured (`capture`), the hooks' automatic recall
/// (`hookRecall`), the MCP tools (`mcp`), or neither (`other`): a direct HTTP client, or a row logged before the
/// client was recorded.
fn channel(client: Option<ClientKind>, write: bool) -> &'static str {
    match client {
        Some(ClientKind::Hook) if write => "capture",
        Some(ClientKind::Hook) => "hookRecall",
        Some(ClientKind::Mcp) => "mcp",
        Some(ClientKind::Http) | None => "other",
    }
}

pub async fn platform_activity(t: &Arc<Instance>) -> ApiResult<Value> {
    let since = util::now().timestamp() - WINDOW_DAYS * 24 * 3600;
    let activity = t.store.client_activity(since).await?;
    let calls = (activity.writes.into_iter().map(|c| (c, true)))
        .chain(activity.recalls.into_iter().map(|c| (c, false)));
    let mut platforms: BTreeMap<String, Platform> = BTreeMap::new();
    for (calls, write) in calls {
        let platform = platforms.entry(calls.platform).or_default();
        platform.all.add(calls.count, &calls.last_at);
        platform
            .channels
            .entry(channel(calls.client, write))
            .or_default()
            .add(calls.count, &calls.last_at);
    }
    let mut platforms: Vec<(String, Platform)> = platforms.into_iter().collect();
    // The platform heard from last comes first.
    platforms.sort_by(|a, b| b.1.all.last_at.cmp(&a.1.all.last_at));
    let rows: Vec<Value> = platforms
        .into_iter()
        .map(|(name, platform)| {
            let mut row = Map::new();
            row.insert("platform".into(), json!(name));
            row.insert("lastAt".into(), json!(platform.all.last_at));
            for (channel, seen) in platform.channels {
                row.insert(
                    channel.into(),
                    json!({"count": seen.count, "lastAt": seen.last_at}),
                );
            }
            Value::Object(row)
        })
        .collect();
    Ok(json!({"window": format!("{WINDOW_DAYS}d"), "platforms": rows}))
}
