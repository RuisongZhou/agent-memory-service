import copy
import json
import math
from pathlib import Path
import struct
import tempfile
import unittest

from model_reference import compare, cosine, fixture, read_json


class ComparisonTests(unittest.TestCase):
    def embedding(self):
        return {"ids":[[0,4,2], [0,5,6,2]], "single":[[1.0,0.0], [0.0,1.0]], "batched":[[1.0,0.0], [0.0,1.0]]}

    def test_vectors_tokens_and_padding_paths(self):
        ref = self.embedding()
        rust = copy.deepcopy(ref)
        self.assertTrue(compare("embed", ref, rust)["ok"])
        rust["batched"][1] = [1.0,0.0]
        self.assertFalse(compare("embed", ref, rust)["ok"])
        rust = copy.deepcopy(ref)
        rust["ids"][1] = [0,5,2]
        self.assertFalse(compare("embed", ref, rust)["ids_equal"])

    def test_cosine_threshold_and_unit_length(self):
        ref = self.embedding()
        rust = copy.deepcopy(ref)
        rust["single"][0] = [0.9995, math.sqrt(1-0.9995**2)]
        rust["batched"][0] = rust["single"][0]
        self.assertTrue(compare("embed", ref, rust)["ok"])
        rust["single"][0] = [0.998, math.sqrt(1-0.998**2)]
        self.assertFalse(compare("embed", ref, rust)["ok"])
        rust = copy.deepcopy(ref); rust["single"][0] = [2.0,0.0]
        self.assertFalse(compare("embed", ref, rust)["checks"]["unit_vectors"]["ok"])

    def test_score_threshold(self):
        ref = {"ids":[[0,4,2,2,5,2]], "single":[0.5], "batched":[0.5]}
        rust = copy.deepcopy(ref); rust["single"][0] = 0.5009
        self.assertTrue(compare("rerank", ref, rust)["ok"])
        rust["batched"][0] = 0.502
        self.assertFalse(compare("rerank", ref, rust)["ok"])

    def test_malformed_or_nonfinite_dumps_fail_closed(self):
        ref = self.embedding()
        for bad in [float("nan"), float("inf"), True, "1"]:
            rust = copy.deepcopy(ref); rust["single"][0][0] = bad
            with self.assertRaises(ValueError): compare("embed", ref, rust)
        rust = copy.deepcopy(ref); rust["batched"].pop()
        with self.assertRaises(ValueError): compare("embed", ref, rust)
        with self.assertRaises(ValueError): cosine([1.0], [1.0,2.0])
        with self.assertRaises(ValueError): cosine([0.0], [0.0])

    def test_fixture_determinism_and_safetensor_offsets(self):
        with tempfile.TemporaryDirectory() as a, tempfile.TemporaryDirectory() as b:
            report = fixture(Path(a)); fixture(Path(b))
            for kind in ["embed", "rerank"]:
                left = Path(a)/f"tiny-{kind}"/"model.safetensors"
                self.assertEqual(left.read_bytes(), (Path(b)/f"tiny-{kind}"/"model.safetensors").read_bytes())
                raw = left.read_bytes(); hlen = struct.unpack("<Q", raw[:8])[0]
                header = json.loads(raw[8:8+hlen]); offset = 0
                for name in sorted(k for k in header if k != "__metadata__"):
                    tensor = header[name]
                    self.assertEqual(tensor["data_offsets"], [offset, offset+math.prod(tensor["shape"])*4])
                    offset = tensor["data_offsets"][1]
                self.assertEqual(len(raw), 8+hlen+offset)
                self.assertLess(report["models"][kind]["bytes"], 100_000)
            self.assertEqual(len(read_json(Path(a)/"texts.json")), 6)
            with self.assertRaises(ValueError): fixture(Path(a))


if __name__ == "__main__":
    unittest.main()
