"""Pure conversion of persisted tenant usage into deduplicatable usage entries."""
from datetime import datetime, timezone



class UsageRecordError(ValueError):
    pass


def usage_entries(records, tenant, tag):
    if not isinstance(records, list):
        raise UsageRecordError(f"{tenant}: llm_usage export must contain records")
    entries = []
    for record in records:
        if not isinstance(record, dict):
            raise UsageRecordError(f"{tenant}: invalid usage record")
        mid = record.get("mid")
        if not isinstance(mid, str) or not mid:
            raise UsageRecordError(f"{tenant}: llm_usage record lacks a stable mid")
        try:
            ts = datetime.fromisoformat(record["createdAt"].replace("Z", "+00:00"))
            if ts.tzinfo is None:
                raise ValueError("timestamp needs timezone")
            hit = int(record.get("cacheHitTokens") or 0)
            prompt = int(record.get("promptTokens") or 0)
            miss = int(record.get("cacheMissTokens", max(prompt - hit, 0)))
            out = int(record.get("completionTokens") or 0)
            if min(hit, prompt, miss, out) < 0:
                raise ValueError("negative usage")
        except (KeyError, AttributeError, TypeError, ValueError) as e:
            raise UsageRecordError(f"{tenant}: invalid usage record {mid}: {e}") from e
        stage = str(record.get("stage") or "unknown")
        entries.append(dict(component=f"{tag}.daemon/{stage}", hit=hit, miss=miss, out=out, ts=ts,
                            model=record.get("model") or "unknown", latency_ms=record.get("latencyMs"),
                            event_id=f"memory/{tenant}/{mid}", tenant=tenant, source_id=mid,
                            source="daemon/llm_usage", source_stage=stage, calls=1,
                            received_at=record.get("receivedAt"),
                            attempt=record.get("attempt"),
                            parse_error=record.get("validJson") is False,
                            collected_at=datetime.now(timezone.utc).isoformat(timespec="seconds")))
    return entries
