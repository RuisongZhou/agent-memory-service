//! Deterministic context token packing.
/// Prefix packing for get_context parts (5.4 "从最旧的一条开始舍弃"): items are newest first; keep the newest
/// prefix that fits. Returns the number of items kept and their cost.
pub fn pack_prefix(costs: &[usize], cap: usize) -> (usize, usize) {
    let mut used = 0;
    for (i, &c) in costs.iter().enumerate() {
        if used + c > cap {
            return (i, used);
        }
        used += c;
    }
    (costs.len(), used)
}
