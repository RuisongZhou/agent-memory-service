import pytest

from common import model_metadata


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


def test_metadata_accepts_the_hosted_api_backend():
    health = {**llama_health(), "modelBackend": "api", "modelDtype": "hosted",
              "modelDevice": "https://maas.qianwenaiapi.com/compatible-mode/v1",
              "embedModel": "qwen3.7-text-embedding-flash", "rerankModel": None}
    metadata = model_metadata.capture(health)
    assert metadata["isMock"] is False and metadata["embedModel"] == "qwen3.7-text-embedding-flash"
    with pytest.raises(ValueError, match="base URL"):
        model_metadata.capture({**health, "modelDevice": "mock"})

