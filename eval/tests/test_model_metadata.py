import copy

import pytest

from common import model_metadata
from common.protocol import guard
import memory_run
from test_harness import args_for
from common import locomo


def llama_health():
    return {"modelBackend": "llama-server", "modelDevice": "http://127.0.0.1:8081", "modelDtype": "Q8_0",
            "embedModel": "bge-m3-Q8_0", "rerankModel": "bge-reranker-v2-m3-Q8_0",
            "promptVariant": "en", "tenantPromptVariants": {}}


def test_actual_metadata_in_protocol_and_runtime_change_rejected(tmp_path):
    health = llama_health()
    args = args_for(tmp_path, "http://unused")
    args.actual_model_runtime = model_metadata.capture(health)
    args.actual_prompt_variant = "en"
    runner = memory_run.Runner(args, None, None, locomo.load()[:1])
    runner.validate_prompt_health(health)
    changed = copy.deepcopy(health)
    changed.update(modelDtype="F16")
    with pytest.raises(memory_run.DaemonError, match="runtime changed"):
        runner.validate_prompt_health(changed)
    args.actual_model_runtime = model_metadata.capture(changed)
    with pytest.raises(RuntimeError, match="model_runtime"):
        memory_run.Runner(args, None, None, locomo.load()[:1])


def test_actual_mock_device_is_explicitly_mock_not_real_bge():
    health = {**llama_health(), "modelBackend": "mock", "modelDevice": "mock",
              "embedModel": "mock-embed", "rerankModel": None}
    metadata = model_metadata.capture(health)
    assert metadata["verified"] is True and metadata["isMock"] is True
    assert metadata["modelDevice"] == "mock" and metadata["modelBackend"] == "mock"
    with pytest.raises(ValueError, match="mock device"):
        model_metadata.capture({**health, "modelDevice": "cpu"})


def test_partial_metadata_and_unknown_backend_fail():
    with pytest.raises(ValueError, match="incomplete"):
        model_metadata.capture({"modelBackend": "llama-server"})
    with pytest.raises(ValueError, match="unrecognized"):
        model_metadata.capture({**llama_health(), "modelBackend": "candle-metal"})


def test_embedding_runtime_uses_same_daemon_health_and_detects_changes(monkeypatch):
    expected = model_metadata.capture(llama_health())
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


def test_llama_server_metadata_records_quantisation_and_url():
    metadata = model_metadata.capture(llama_health())
    assert metadata["verified"] is True and metadata["isMock"] is False
    assert metadata["modelDtype"] == "Q8_0"
    with pytest.raises(ValueError, match="GGUF file type"):
        model_metadata.capture({**llama_health(), "modelDtype": "unknown"})
    with pytest.raises(ValueError, match="server URL"):
        model_metadata.capture({**llama_health(), "modelDevice": "metal:1"})
    with pytest.raises(ValueError, match="runtime changed"):
        model_metadata.assert_same(metadata, {**llama_health(), "modelDtype": "F16"})
