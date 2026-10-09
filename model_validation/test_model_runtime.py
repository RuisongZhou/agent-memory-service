import copy
import json
from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import model_reference
from model_runtime import GpuSampler, inspect_runtime, parse_runtime


class RuntimeTests(unittest.TestCase):
    def test_real_registry_id_and_f32_are_accepted(self):
        runtime = {"backend": "candle-metal", "device": "metal:4294968527", "dtype": "float32"}
        self.assertTrue(inspect_runtime(runtime, "metal")["metalVerified"])
        self.assertFalse(inspect_runtime({**runtime, "device": "cpu"}, "metal")["verified"])
        self.assertFalse(inspect_runtime({**runtime, "dtype": "float16"}, "metal")["verified"])
        self.assertFalse(inspect_runtime({}, "metal")["verified"])

    def test_auto_cpu_is_not_a_metal_success(self):
        checked = inspect_runtime({"backend": "apple-accelerate", "device": "cpu", "dtype": "f32"}, "auto")
        self.assertTrue(checked["verified"])
        self.assertFalse(checked["metalVerified"])

    def test_runtime_json_has_priority_over_legacy_text(self):
        text = 'backend candle-gemm\nruntime {"backend":"candle-metal","device":"metal:4294968527","dtype":"float32"}\n'
        self.assertEqual(parse_runtime(text)["backend"], "candle-metal")
        self.assertEqual(parse_runtime("backend candle-metal | device metal:1 | dtype float32")["device"], "metal:1")

    def test_gpu_memory_unknown_is_not_zero(self):
        report = GpuSampler(0).stop()
        self.assertIsNone(report["processGpuMemoryPeakBytes"])
        self.assertIn("does not cover all Metal", report["memoryNote"])

    def test_cached_cpu_metal_orchestration_and_no_silent_fallback(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            model = root / "model"
            model.mkdir()
            cache = root / "cpu"
            cache.mkdir()
            vectors = [[1., 0.] for _ in model_reference.TEXTS]
            base = {"model": str(model), "device": "cpu", "backend": "torch-cpu", "dtype": "float32", "torch": "test",
                    "ids": [[0, 2] for _ in vectors], "single": vectors, "batched": vectors}
            model_reference.write_json(cache / "embed-inputs.json", model_reference.TEXTS)
            model_reference.write_json(cache / "embed-reference.json", base)
            model_reference.write_json(cache / "embed-rust.json", {**base, "backend": "candle-gemm"})
            commands = []
            selected = {"device": "metal:4294968527", "backend": "candle-metal", "dtype": "float32"}
            def fake_run(command, **kwargs):
                commands.append(command)
                output = Path(command[4])
                model_reference.write_json(output, {**base, **selected})
                return SimpleNamespace(returncode=0)
            args = SimpleNamespace(output=root / "metal", embed_dir=model, rerank_dir=None,
                                   bench=root / "fake-bench", stress=False, device="metal", cpu_baseline_dir=cache)
            with patch.object(model_reference.subprocess, "run", side_effect=fake_run):
                report = model_reference.run(args)
            self.assertTrue(report["ok"])
            self.assertTrue(report["models"]["embed"]["metal_acceptance_passed"])
            self.assertEqual(len(commands), 1)  # cached Torch/CPU paths are never reloaded
            self.assertEqual(commands[0][-2:], ["--device", "metal"])
            self.assertTrue((args.output / "embed-rust-cpu.json").exists())
            selected.update(device="cpu", backend="candle-gemm")
            args.output = root / "fallback"
            with patch.object(model_reference.subprocess, "run", side_effect=fake_run):
                report = model_reference.run(args)
            self.assertFalse(report["ok"])


if __name__ == "__main__":
    unittest.main()
