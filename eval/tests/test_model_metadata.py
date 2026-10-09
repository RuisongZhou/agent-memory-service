import copy

import pytest

from common import model_metadata
from common.protocol import guard
import memory_run
from test_harness import args_for
from common import locomo


def metal_health():
    return {"modelBackend": "candle-metal", "modelDevice": "metal:4294968527", "modelDtype": "f32",
            "embedModel": "BAAI/bge-m3", "rerankModel": "BAAI/bge-reranker-v2-m3",
            "promptVariant": "en", "tenantPromptVariants": {}}


def test_actual_metal_metadata_in_protocol_and_runtime_change_rejected(tmp_path):
    health = metal_health()
    args = args_for(tmp_path, "http://unused")
    args.actual_model_runtime = model_metadata.capture(health)
    args.actual_prompt_variant = "en"
    runner = memory_run.Runner(args, None, None, locomo.load()[:1])
    runner.validate_prompt_health(health)
    changed = copy.deepcopy(health)
    changed.update(modelBackend="apple-accelerate", modelDevice="cpu")
    with pytest.raises(memory_run.DaemonError, match="runtime changed"):
        runner.validate_prompt_health(changed)
    args.actual_model_runtime = model_metadata.capture(changed)
    with pytest.raises(RuntimeError, match="model_runtime"):
        memory_run.Runner(args, None, None, locomo.load()[:1])


def test_legacy_unknown_preserves_known_model_ids():
    metadata = model_metadata.capture({"embedModel": "old-embed", "rerankModel": None})
    assert metadata["schema"] == "legacy-unknown" and metadata["verified"] is False
    assert metadata["modelBackend"] is None and metadata["embedModel"] == "old-embed"


def test_actual_mock_device_is_explicitly_mock_not_real_bge():
    health = {**metal_health(), "modelBackend": "mock", "modelDevice": "mock",
              "embedModel": "mock-embed", "rerankModel": None}
    metadata = model_metadata.capture(health)
    assert metadata["verified"] is True and metadata["isMock"] is True
    assert metadata["modelDevice"] == "mock" and metadata["modelBackend"] == "mock"
    assert model_metadata.capture({**health, "modelDevice": "cpu"})["isMock"] is True


def test_partial_new_metadata_and_metal_cpu_mismatch_fail():
    with pytest.raises(ValueError, match="incomplete"):
        model_metadata.capture({"modelBackend": "candle-metal"})
    with pytest.raises(ValueError, match="actual metal"):
        model_metadata.capture({**metal_health(), "modelDevice": "cpu"})


def test_embedding_runtime_uses_same_daemon_health_and_detects_changes(monkeypatch):
    expected = model_metadata.capture(metal_health())
    urls = []
    def fetch(url):
        urls.append(url)
        return expected
    monkeypatch.setattr(model_metadata, "fetch_embedding_runtime", fetch)
    model_metadata.check_embedding_runtime("http://127.0.0.1:8765/v1", expected)
    assert model_metadata.health_base(urls[0]) == "http://127.0.0.1:8765"
    monkeypatch.setattr(model_metadata, "fetch_embedding_runtime", lambda _: {**expected, "embedModel": "another-model"})
    with pytest.raises(ValueError, match="embedding daemon"):
        model_metadata.check_embedding_runtime(urls[0], expected)
