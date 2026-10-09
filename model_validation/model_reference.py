#!/usr/bin/env python3
"""Offline XLM-R correctness checks with CPU/f32 references and explicit Metal acceptance.

`run` launches reference and Rust processes sequentially, keeping one model resident at a time.
`fixture`, `compare` and the comparator tests require only the Python standard library.
"""
from __future__ import annotations

import argparse
import gc
import json
import math
import os
from pathlib import Path
import random
import re
import struct
import subprocess
import sys
from model_runtime import inspect_runtime

TEXTS = [
    "Caroline joined a support group yesterday.",
    "Melanie likes painting sunsets and lakes after work.",
    "The deployment service port changed from 8100 to 8200 last week.",
    "昨天我参加了社区读书会。",
    "用户偏好简洁的中文说明，并希望保留原始出处。",
    "测试环境的服务端口已经从8100改为8200。",
]
PAIRS = [
    ["When did Caroline join the support group?", TEXTS[0]],
    ["When did Caroline join the support group?", TEXTS[1]],
    ["What is the deployment service port?", TEXTS[2]],
    ["测试环境的服务端口是多少？", TEXTS[5]],
    ["测试环境的服务端口是多少？", TEXTS[3]],
    ["用户喜欢怎样的说明？", TEXTS[4]],
]
COSINE_MIN = 0.999
SCORE_DIFF_MAX = 0.001


def write_json(path: Path, value) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(value, ensure_ascii=False, indent=2, allow_nan=False) + "\n", encoding="utf-8")


def read_json(path: Path):
    def invalid(value):
        raise ValueError(f"non-finite JSON number: {value}")
    return json.loads(path.read_text(encoding="utf-8"), parse_constant=invalid)


def finite_number(value) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
        raise ValueError(f"expected finite number, got {value!r}")
    return float(value)


def cosine(left, right) -> float:
    if not isinstance(left, list) or not isinstance(right, list) or not left or len(left) != len(right):
        raise ValueError("embedding dimensions differ or are empty")
    left, right = [finite_number(x) for x in left], [finite_number(x) for x in right]
    ln = math.sqrt(math.fsum(x*x for x in left))
    rn = math.sqrt(math.fsum(x*x for x in right))
    if ln == 0 or rn == 0:
        raise ValueError("zero embedding cannot be compared by cosine")
    return max(-1.0, min(1.0, math.fsum(a*b for a, b in zip(left, right)) / ln / rn))


def compare(kind: str, reference: dict, rust: dict) -> dict:
    """Compare tokens and all single/batched paths. Fail closed on malformed or NaN dumps."""
    if kind not in ["embed", "rerank"] or not isinstance(reference, dict) or not isinstance(rust, dict):
        raise ValueError("expected embed/rerank kind and object-shaped dumps")
    ids = reference.get("ids")
    if not isinstance(ids, list) or not ids:
        raise ValueError("reference requires a nonempty ids array")
    for sequences in [ids, rust.get("ids")]:
        if not isinstance(sequences, list) or any(not isinstance(row, list) or not row
                or any(type(token) is not int or token < 0 for token in row) for row in sequences):
            raise ValueError("ids must contain nonempty unsigned token-id sequences")
    count = len(ids)
    for name, dump in [("reference", reference), ("rust", rust)]:
        for field in ["single", "batched"]:
            if not isinstance(dump.get(field), list) or len(dump[field]) != count:
                raise ValueError(f"{name}.{field} count differs from token count {count}")
    if kind == "embed":
        dimensions = {len(row) for dump in [reference, rust] for field in ["single", "batched"]
            for row in dump[field] if isinstance(row, list)}
        if len(dimensions) != 1 or not next(iter(dimensions)):
            raise ValueError("embedding dimensions are inconsistent across rows/paths")
    comparisons = [
        ("rust_single_vs_reference_single", rust["single"], reference["single"]),
        ("rust_batched_vs_reference_batched", rust["batched"], reference["batched"]),
        ("rust_single_vs_batched", rust["single"], rust["batched"]),
        ("reference_single_vs_batched", reference["single"], reference["batched"]),
    ]
    checks = {}
    for name, a, b in comparisons:
        values = [cosine(x, y) if kind == "embed" else abs(finite_number(x)-finite_number(y)) for x, y in zip(a, b)]
        passed = all(v >= COSINE_MIN for v in values) if kind == "embed" else all(v <= SCORE_DIFF_MAX for v in values)
        checks[name] = {"ok": passed, "values": values,
            "min_cosine" if kind == "embed" else "max_score_diff": min(values) if kind == "embed" else max(values)}
    norms_ok = True
    if kind == "embed":
        norms = {}
        for name, dump in [("reference", reference), ("rust", rust)]:
            for field in ["single", "batched"]:
                n = [math.sqrt(math.fsum(finite_number(x)**2 for x in row)) for row in dump[field]]
                norms[f"{name}_{field}"] = n
                norms_ok &= all(abs(value-1) <= 0.001 for value in n)
        checks["unit_vectors"] = {"ok": norms_ok, "norms": norms}
    else:
        scores_ok = all(0 <= finite_number(value) <= 1 for dump in [reference, rust]
            for field in ["single", "batched"] for value in dump[field])
        checks["probabilities"] = {"ok": scores_ok}
    ids_ok = ids == rust["ids"]
    return {"kind": kind, "ok": ids_ok and all(c["ok"] for c in checks.values()), "count": count,
        "ids_equal": ids_ok, "thresholds": {"min_cosine": COSINE_MIN, "max_score_diff": SCORE_DIFF_MAX}, "checks": checks}


def reference(kind: str, model_dir: Path, inputs: list, device: str = "cpu") -> dict:
    # Absolute local paths + local_files_only prevent implicit model/tokenizer downloads.
    if not model_dir.is_dir():
        raise ValueError(f"model directory does not exist: {model_dir}")
    if device not in ("cpu", "metal"):
        raise ValueError("Torch reference device must be cpu or metal")
    if device == "metal":
        os.environ["PYTORCH_ENABLE_MPS_FALLBACK"] = "0"
        os.environ["PYTORCH_MPS_FAST_MATH"] = "0"
    import torch
    from transformers import AutoModel, AutoModelForSequenceClassification, PreTrainedTokenizerFast
    torch.set_num_threads(int(os.environ.get("OMP_NUM_THREADS", "2")))
    torch.set_float32_matmul_precision("highest")
    if device == "metal" and not torch.backends.mps.is_available():
        raise ValueError("explicit Metal reference requested but Torch MPS is unavailable; no CPU fallback")
    actual_device = torch.device("mps" if device == "metal" else "cpu")
    cfg = read_json(model_dir / "config.json")
    max_length = min(512, int(cfg["max_position_embeddings"])-int(cfg["pad_token_id"])-1)
    tokenizer = PreTrainedTokenizerFast(tokenizer_file=str(model_dir / "tokenizer.json"),
        bos_token="<s>", eos_token="</s>", sep_token="</s>", cls_token="<s>", pad_token="<pad>", unk_token="<unk>")
    model_class = AutoModel if kind == "embed" else AutoModelForSequenceClassification
    options = {"local_files_only": True, "attn_implementation": "eager", "low_cpu_mem_usage": True}
    if kind == "embed":
        options["add_pooling_layer"] = False
    model = model_class.from_pretrained(str(model_dir), **options).float().to(actual_device).eval()

    def encode(items):
        if kind == "embed":
            encoded = tokenizer(items, truncation="longest_first", max_length=max_length, padding=True, return_tensors="pt")
        else:
            encoded = tokenizer([p[0] for p in items], [p[1] for p in items],
                truncation="longest_first", max_length=max_length, padding=True, return_tensors="pt")
        # XLM-R uses token type 0 for every token, including the second sequence.
        encoded.pop("token_type_ids", None)
        encoded = {key: value.to(actual_device) for key, value in encoded.items()}
        with torch.inference_mode():
            result = model(**encoded)
            if kind == "embed":
                vectors = torch.nn.functional.normalize(result.last_hidden_state[:, 0], p=2, dim=1)
                return vectors.detach().cpu().tolist()
            return torch.sigmoid(result.logits.float().reshape(-1)).detach().cpu().tolist()

    if not inputs:
        raise ValueError("inputs must be nonempty")
    single = [encode([item])[0] for item in inputs]
    if kind == "embed":
        batched = encode(inputs)
        tokens = tokenizer(inputs, truncation="longest_first", max_length=max_length, padding=False)["input_ids"]
    else:
        batched = [None] * len(inputs)
        for query in dict.fromkeys(p[0] for p in inputs):
            indexes = [i for i, pair in enumerate(inputs) if pair[0] == query]
            for index, score in zip(indexes, encode([inputs[i] for i in indexes])):
                batched[index] = score
        tokens = tokenizer([p[0] for p in inputs], [p[1] for p in inputs],
            truncation="longest_first", max_length=max_length, padding=False)["input_ids"]
    result = {"model": str(model_dir), "kind": kind, "device": str(actual_device), "requested_device": device,
        "backend": "torch-mps" if device == "metal" else "torch-cpu", "dtype": "float32",
        "torch": torch.__version__, "single": single, "batched": batched, "ids": tokens, "max_length": max_length}
    if device == "metal":
        torch.mps.synchronize()
        result["mps_memory_snapshot"] = {"currentAllocatedBytes": torch.mps.current_allocated_memory(),
                                       "driverAllocatedBytes": torch.mps.driver_allocated_memory(),
                                       "note": "snapshot with weights resident; not a peak or total system allocation"}
    del model
    gc.collect()
    return result


def safetensors(path: Path, tensors: dict) -> None:
    """Write little-endian F32 tensors without a Torch/safetensors dependency."""
    header, chunks, offset = {"__metadata__": {"format": "pt"}}, [], 0
    for name in sorted(tensors):
        shape, values = tensors[name]
        assert math.prod(shape) == len(values)
        data = struct.pack(f"<{len(values)}f", *values)
        header[name] = {"dtype": "F32", "shape": shape, "data_offsets": [offset, offset+len(data)]}
        offset += len(data)
        chunks.append(data)
    metadata = json.dumps(header, separators=(",", ":")).encode()
    metadata += b" " * ((-len(metadata)) % 8)
    with path.open("wb") as output:
        output.write(struct.pack("<Q", len(metadata)))
        output.write(metadata)
        for data in chunks:
            output.write(data)


def fixture(output: Path) -> dict:
    """Two deterministic tiny XLM-R checkpoints (~40 KiB each), with a real tokenizer pipeline."""
    words = set()
    for text in TEXTS + [q for q, _ in PAIRS]:
        words.update(re.findall(r"[\u4e00-\u9fff]|[a-z]+|\d+|[^\w\s]", text.lower()))
    vocab = {token: i for i, token in enumerate(["<s>", "<pad>", "</s>", "<unk>"] + sorted(words))}
    tokenizer = {"version":"1.0", "truncation":None, "padding":None,
        "added_tokens":[{"id":i,"content":token,"single_word":False,"lstrip":False,"rstrip":False,"normalized":False,"special":True}
            for token, i in list(vocab.items())[:4]],
        "normalizer":{"type":"Lowercase"},
        "pre_tokenizer":{"type":"Sequence", "pretokenizers":[
            {"type":"Split", "pattern":{"Regex":"[\u4e00-\u9fff]"}, "behavior":"Isolated", "invert":False},
            {"type":"Whitespace"}]},
        "post_processor":{"type":"RobertaProcessing","sep":["</s>",2],"cls":["<s>",0],"trim_offsets":True,"add_prefix_space":False},
        "decoder":None, "model":{"type":"WordLevel", "vocab":vocab, "unk_token":"<unk>"}}
    cfg = {"model_type":"xlm-roberta", "hidden_size":16, "num_attention_heads":4, "num_hidden_layers":2,
        "intermediate_size":32, "layer_norm_eps":1e-5, "pad_token_id":1, "bos_token_id":0, "eos_token_id":2,
        "max_position_embeddings":64, "vocab_size":len(vocab), "type_vocab_size":1,
        "hidden_act":"gelu", "position_embedding_type":"absolute", "hidden_dropout_prob":0.0,
        "attention_probs_dropout_prob":0.0, "classifier_dropout":0.0, "num_labels":1, "id2label":{"0":"LABEL_0"},
        "label2id":{"LABEL_0":0}}
    report = {"seed":146, "models":{}}
    for kind in ["embed", "rerank"]:
        directory = output / f"tiny-{kind}"
        if (directory / "model.safetensors").exists():
            raise ValueError(f"refusing to replace an existing checkpoint: {directory}")
        directory.mkdir(parents=True, exist_ok=True)
        random_source = random.Random(146)
        tensors = {}
        prefix = "" if kind == "embed" else "roberta."
        def weights(name, shape, scale=0.2, base=0.0):
            values = [base+random_source.uniform(-scale, scale) for _ in range(math.prod(shape))]
            tensors[prefix+name] = (shape, values)
        weights("embeddings.word_embeddings.weight", [len(vocab),16], scale=0.5)
        tensors[prefix+"embeddings.word_embeddings.weight"][1][16:32] = [0.0]*16
        weights("embeddings.position_embeddings.weight", [64,16])
        weights("embeddings.token_type_embeddings.weight", [1,16])
        weights("embeddings.LayerNorm.weight", [16], scale=0.05, base=1)
        weights("embeddings.LayerNorm.bias", [16], scale=0.02)
        for layer in range(2):
            root = f"encoder.layer.{layer}."
            for component in ["query", "key", "value"]:
                weights(root+f"attention.self.{component}.weight", [16,16])
                weights(root+f"attention.self.{component}.bias", [16], scale=0.02)
            for component, shape in [("attention.output.dense",[16,16]), ("intermediate.dense",[32,16]), ("output.dense",[16,32])]:
                weights(root+component+".weight", shape)
                weights(root+component+".bias", [shape[0]], scale=0.02)
            for component in ["attention.output.LayerNorm", "output.LayerNorm"]:
                weights(root+component+".weight", [16], scale=0.05, base=1)
                weights(root+component+".bias", [16], scale=0.02)
        if kind == "rerank":
            for component, shape in [("dense",[16,16]), ("out_proj",[1,16])]:
                tensors[f"classifier.{component}.weight"] = (shape, [random_source.uniform(-0.2,0.2) for _ in range(math.prod(shape))])
                tensors[f"classifier.{component}.bias"] = ([shape[0]], [random_source.uniform(-0.02,0.02) for _ in range(shape[0])])
        config = dict(cfg, architectures=["XLMRobertaModel" if kind == "embed" else "XLMRobertaForSequenceClassification"])
        write_json(directory / "config.json", config)
        write_json(directory / "tokenizer.json", tokenizer)
        write_json(directory / "tokenizer_config.json", {"tokenizer_class":"PreTrainedTokenizerFast", "model_max_length":62,
            "bos_token":"<s>", "eos_token":"</s>", "pad_token":"<pad>", "unk_token":"<unk>"})
        safetensors(directory / "model.safetensors", tensors)
        report["models"][kind] = {"path":str(directory), "bytes":(directory / "model.safetensors").stat().st_size, "tensors":len(tensors)}
    write_json(output / "texts.json", TEXTS)
    write_json(output / "pairs.json", PAIRS)
    return report


def run(args) -> dict:
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    requested_device = getattr(args, "device", "cpu")
    baseline_dir = getattr(args, "cpu_baseline_dir", None)
    report = {"ok":True, "mode":"offline CPU/f32 reference", "requested_device":requested_device, "models":{}}
    script = Path(__file__).resolve()
    for kind in ["embed", "rerank"]:
        directory = getattr(args, f"{kind}_dir")
        if directory is None:
            continue
        inputs = list(TEXTS) if kind == "embed" else [list(p) for p in PAIRS]
        if args.stress:
            inputs.extend(["", " ".join(TEXTS)*100] if kind == "embed" else [[PAIRS[0][0], ""], [PAIRS[0][0], " ".join(TEXTS)*100]])
        inp, ref, rust = output / f"{kind}-inputs.json", output / f"{kind}-reference.json", output / f"{kind}-rust.json"
        write_json(inp, inputs)
        cpu_dump = output / f"{kind}-rust-cpu.json"
        if baseline_dir:
            cached_inputs = baseline_dir / f"{kind}-inputs.json"
            cached_ref = baseline_dir / f"{kind}-reference.json"
            cached_cpu = baseline_dir / f"{kind}-rust.json"
            if read_json(cached_inputs) != inputs:
                raise ValueError("cached CPU baseline inputs differ from the selected six/stress inputs")
            reference_data = read_json(cached_ref)
            if reference_data.get("device") != "cpu" or reference_data.get("dtype") != "float32":
                raise ValueError("cached reference must be actual CPU/float32")
            if Path(reference_data.get("model", "")).resolve() != directory.resolve():
                raise ValueError("cached CPU reference model directory differs")
            weights = [directory / name for name in ("config.json", "tokenizer.json", "model.safetensors", "pytorch_model.bin")
                       if (directory / name).exists()]
            cache_cutoff = max(cached_ref.stat().st_mtime_ns, cached_cpu.stat().st_mtime_ns)
            if any(path.stat().st_mtime_ns > cache_cutoff for path in weights):
                raise ValueError("model/tokenizer files changed after cached CPU reference")
            cpu_data = read_json(cached_cpu)
            if not compare(kind, reference_data, cpu_data)["ok"]:
                raise ValueError("cached CPU output does not pass the Torch baseline")
            write_json(ref, reference_data)
            write_json(cpu_dump, cpu_data)
        else:
            # Explicit subprocess boundaries release each model before loading the next device.
            subprocess.run([sys.executable, str(script), "reference", kind, str(directory.resolve()), str(inp), str(ref),
                            "--device", "cpu"], check=True, stdout=subprocess.PIPE)
            if requested_device != "cpu":
                subprocess.run([str(args.bench.resolve()), f"dump-{kind}", str(directory.resolve()), str(inp), str(cpu_dump),
                                "--device", "cpu"], check=True)
        subprocess.run([str(args.bench.resolve()), f"dump-{kind}", str(directory.resolve()), str(inp), str(rust),
                        "--device", requested_device], check=True)
        rust_data = read_json(rust)
        compared = compare(kind, read_json(ref), rust_data)
        runtime = inspect_runtime(rust_data, requested_device)
        compared["runtime"] = runtime
        compared["ok"] &= runtime["verified"]
        if requested_device != "cpu":
            cpu_data = read_json(cpu_dump)
            compared["metal_or_auto_vs_cpu"] = compare(kind, cpu_data, rust_data)
            compared["cpu_baseline_runtime"] = inspect_runtime(cpu_data, "cpu")
            compared["cpu_baseline_cached"] = str(baseline_dir) if baseline_dir else None
            compared["ok"] &= compared["metal_or_auto_vs_cpu"]["ok"]
            if not baseline_dir or cpu_data.get("device") is not None:
                compared["ok"] &= compared["cpu_baseline_runtime"]["verified"]
        compared["metal_acceptance_passed"] = compared["ok"] and runtime["metalVerified"]
        report["models"][kind] = compared
        report["ok"] &= compared["ok"]
    if not report["models"]:
        raise ValueError("provide --embed-dir and/or --rerank-dir")
    return report


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    p = commands.add_parser("fixture", help="generate tiny checkpoints; no Torch/download required")
    p.add_argument("output", type=Path)
    p = commands.add_parser("reference", help="generate a Torch/Transformers reference dump from local weights")
    p.add_argument("kind", choices=["embed", "rerank"])
    p.add_argument("model_dir", type=Path); p.add_argument("input", type=Path); p.add_argument("output", type=Path)
    p.add_argument("--device", choices=["cpu", "metal"], default="cpu")
    p = commands.add_parser("compare", help="compare existing dumps; no Torch required")
    p.add_argument("kind", choices=["embed", "rerank"])
    p.add_argument("reference", type=Path); p.add_argument("rust", type=Path); p.add_argument("output", type=Path)
    p.add_argument("--device", choices=["cpu", "metal", "auto"], default=None, help="also require actual f32 runtime evidence")
    p = commands.add_parser("run", help="CPU/f32 reference then explicit Rust device, one process/model at a time")
    p.add_argument("--bench", type=Path, required=True)
    p.add_argument("--embed-dir", type=Path); p.add_argument("--rerank-dir", type=Path)
    p.add_argument("--output", type=Path, required=True)
    p.add_argument("--stress", action="store_true", help="include empty text/document and long truncation cases")
    p.add_argument("--device", choices=["cpu", "metal", "auto"], default="cpu")
    p.add_argument("--cpu-baseline-dir", type=Path, default=None, help="reuse matching validated CPU reference/dump; inputs and local model must match")
    args = parser.parse_args()
    try:
        if args.command == "fixture":
            result = fixture(args.output.resolve())
        elif args.command == "reference":
            result = reference(args.kind, args.model_dir.resolve(), read_json(args.input), args.device)
            write_json(args.output, result)
            result = {"ok":True, "output":str(args.output), "count":len(result["ids"])}
        elif args.command == "compare":
            rust_data = read_json(args.rust)
            result = compare(args.kind, read_json(args.reference), rust_data)
            if args.device:
                result["runtime"] = inspect_runtime(rust_data, args.device)
                result["ok"] &= result["runtime"]["verified"]
                result["metal_acceptance_passed"] = result["ok"] and result["runtime"]["metalVerified"]
            write_json(args.output, result)
        else:
            result = run(args)
            write_json(args.output / "report.json", result)
        print(json.dumps(result, ensure_ascii=False, allow_nan=False))
        return 0 if result.get("ok", True) else 1
    except Exception as error:
        result = {"ok":False, "error":f"{type(error).__name__}: {error}"}
        if args.command in ["compare", "run"]:
            write_json(args.output if args.command == "compare" else args.output / "report.json", result)
        print(json.dumps(result, ensure_ascii=False), file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
