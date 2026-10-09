"""Capture actual daemon model/device provenance; no model inference or paid API calls."""
from __future__ import annotations

from datetime import datetime, timezone
import json
from pathlib import Path

FIELDS = ("modelBackend", "modelDevice", "modelDtype", "embedModel", "rerankModel")


def capture(health: dict) -> dict:
    values = {key: health.get(key) for key in FIELDS}
    if "modelBackend" not in health:
        return {"schema": "legacy-unknown", "verified": False, "isMock": None, **values}
    missing = [key for key in FIELDS if key not in health]
    if missing:
        raise ValueError(f"new daemon model metadata is incomplete: {', '.join(missing)}")
    backend, device, dtype = values["modelBackend"], values["modelDevice"], values["modelDtype"]
    if backend not in ("candle-metal", "candle-gemm", "apple-accelerate", "mock"):
        raise ValueError(f"unrecognized actual model backend {backend!r}")
    if str(dtype).lower() not in ("f32", "float32", "fp32"):
        raise ValueError("actual daemon model dtype must be f32")
    if backend == "candle-metal":
        if not isinstance(device, str) or not device.startswith("metal:"):
            raise ValueError("Metal backend must report its actual metal registry/device id")
    elif backend == "mock":
        if device not in ("mock", "cpu"):
            raise ValueError("mock backend must report mock or legacy cpu device")
    elif device != "cpu":
        raise ValueError("CPU backend must report actual cpu device")
    if not isinstance(values["embedModel"], str) or not values["embedModel"]:
        raise ValueError("actual embedModel is missing")
    if values["rerankModel"] is not None and not isinstance(values["rerankModel"], str):
        raise ValueError("rerankModel must be a model id or null")
    return {"schema": "actual-runtime-v1", "verified": True, "isMock": backend == "mock", **values}


def assert_same(expected: dict, health: dict) -> dict:
    actual = capture(health)
    if actual != expected:
        changed = [key for key in actual if actual.get(key) != expected.get(key)]
        raise ValueError(f"actual model runtime changed: {', '.join(changed)}; use a new run tag/store after validation")
    return actual


def health_base(embed_url: str) -> str:
    value = embed_url.rstrip("/")
    return value[:-3] if value.endswith("/v1") else value


def fetch_embedding_runtime(embed_url: str) -> dict:
    import httpx
    with httpx.Client(timeout=30, trust_env=False) as client:
        response = client.get(health_base(embed_url) + "/health")
        response.raise_for_status()
        return capture(response.json())


def check_embedding_runtime(embed_url: str, expected: dict) -> None:
    actual = fetch_embedding_runtime(embed_url)
    if actual != expected:
        raise ValueError("actual embedding daemon models/device/dtype changed during mem0 evaluation")


def snapshot(path: Path, runtime: dict, source: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps({"capturedAt": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                               "source": source, "runtime": runtime}, indent=2) + "\n", encoding="utf-8")
