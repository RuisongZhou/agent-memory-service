"""Capture actual daemon model/device provenance; no model inference or paid API calls."""
from __future__ import annotations

from datetime import datetime, timezone
import json
from pathlib import Path

FIELDS = ("modelBackend", "modelDevice", "modelDtype", "embedModel", "rerankModel")


def capture(health: dict) -> dict:
    missing = [key for key in FIELDS if key not in health]
    if missing:
        raise ValueError(f"daemon model metadata is incomplete: {', '.join(missing)}")
    values = {key: health[key] for key in FIELDS}
    backend, device, dtype = values["modelBackend"], values["modelDevice"], values["modelDtype"]
    if backend == "llama-server":
        # GGUF file type reported by the server, e.g. Q8_0 or F16; device is the embedding server URL.
        if not isinstance(dtype, str) or not dtype or dtype == "unknown":
            raise ValueError("llama-server backend must report its GGUF file type")
        if not isinstance(device, str) or not device.startswith(("http://", "https://")):
            raise ValueError("llama-server backend must report its embedding server URL")
    elif backend == "mock":
        if device != "mock":
            raise ValueError("mock backend must report the mock device")
    else:
        raise ValueError(f"unrecognized actual model backend {backend!r}")
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
