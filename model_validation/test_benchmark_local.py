from pathlib import Path
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

import benchmark_local


class BenchmarkWrapperTests(unittest.TestCase):
    def fake_process(self, stdout, returncode=0):
        import io
        return SimpleNamespace(pid=99, stdout=io.StringIO(stdout), stderr=io.StringIO(""), returncode=returncode)

    def test_metal_workload_runtime_and_resource_limits(self):
        stdout = ('runtime {"backend":"candle-metal","device":"metal:4294968527","dtype":"float32"}\n'
                  'model BAAI/bge-m3 | load 1.20s\n'
                  'embed: 0.58 s/batch mean (median 0.57, best 0.56) → 55.2 texts/s (median 56)\n')
        usage = SimpleNamespace(ru_maxrss=1234, ru_utime=.3, ru_stime=.1)
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(benchmark_local.subprocess, "Popen", return_value=self.fake_process(stdout)) as start, \
                 patch.object(benchmark_local.os, "wait4", return_value=(99, 0, usage)), \
                 patch.object(benchmark_local, "hardware_snapshot", return_value={"system": "test"}):
                report = benchmark_local.benchmark(Path("fake"), "embed", Path(tmp), 8, 3, Path(tmp), "metal", 0)
        command = start.call_args.args[0]
        self.assertIn("--device", command)
        self.assertEqual(command[command.index("--device") + 1], "metal")
        self.assertEqual(command[command.index("--batch") + 1], "32")
        self.assertEqual(report["warmup"], 1)
        self.assertEqual(report["rounds"], 3)
        self.assertTrue(report["ok"] and report["runtime"]["metalVerified"])
        self.assertEqual(report["textsPerSecond"], 55.2)
        self.assertIsNone(report["gpuResources"]["processGpuMemoryPeakBytes"])
        self.assertIn("does not include all Metal", report["memoryMeasurementNote"])

    def test_cpu_fallback_cannot_pass_metal_benchmark(self):
        stdout = ('runtime {"backend":"apple-accelerate","device":"cpu","dtype":"float32"}\n'
                  'load 1.2s\nrerank 30 pairs: 637 ms mean\n')
        usage = SimpleNamespace(ru_maxrss=100, ru_utime=.1, ru_stime=.1)
        with tempfile.TemporaryDirectory() as tmp:
            with patch.object(benchmark_local.subprocess, "Popen", return_value=self.fake_process(stdout)), \
                 patch.object(benchmark_local.os, "wait4", return_value=(99, 0, usage)), \
                 patch.object(benchmark_local, "hardware_snapshot", return_value={}):
                report = benchmark_local.benchmark(Path("fake"), "rerank", Path(tmp), 8, 3, Path(tmp), "metal", 0)
        self.assertFalse(report["ok"] or report["targetMet"])
        self.assertEqual(report["meanLatencyMs"], 637)


if __name__ == "__main__":
    unittest.main()
