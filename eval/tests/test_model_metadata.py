import pytest

from common import locomo, model_metadata
import memory_run
from test_harness import args_for


def llama_health():
    return {"modelBackend": "llama-server", "modelDevice": "http://127.0.0.1:8081", "modelDtype": "Q8_0",
            "embedModel": "bge-m3-Q8_0", "rerankModel": "bge-reranker-v2-m3-Q8_0",
            "promptVariant": "en", "tenantPromptVariants": {}}


def test_metadata_distinguishes_llama_server_from_mock():
    metadata = model_metadata.capture(llama_health())
    assert metadata["verified"] is True and metadata["isMock"] is False
    assert metadata["modelDtype"] == "Q8_0"
    mock = model_metadata.capture({**llama_health(), "modelBackend": "mock", "modelDevice": "mock",
                                   "embedModel": "mock-embed", "rerankModel": None})
    assert mock["isMock"] is True


def test_runtime_change_during_a_run_is_rejected(tmp_path):
    args = args_for(tmp_path, "http://unused")
    args.actual_model_runtime = model_metadata.capture(llama_health())
    args.actual_prompt_variant = "en"
    runner = memory_run.Runner(args, None, None, locomo.load()[:1])
    runner.validate_prompt_health(llama_health())
    with pytest.raises(memory_run.DaemonError, match="runtime changed"):
        runner.validate_prompt_health({**llama_health(), "modelDtype": "F16"})
