//! Reference time parsing shared by adapters and memory algorithms.
use crate::domain::types::*;
use chrono::{DateTime, FixedOffset};
use memory_rules::util;

/// Reference "now": `asOf` (eval extension; RFC3339 or YYYY-MM-DD = UTC midnight) or the local clock.
pub fn ref_now(as_of: Option<&str>) -> ApiResult<DateTime<FixedOffset>> {
    match as_of.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(chrono::Local::now().fixed_offset()),
        Some(s) => {
            if let Some(t) = util::parse_ts_fixed(s) {
                return Ok(t);
            }
            util::parse_ts(s)
                .map(|t| t.fixed_offset())
                .ok_or_else(|| ApiError::InvalidArgument(format!("asOf is not RFC3339: {s:?}")))
        }
    }
}
