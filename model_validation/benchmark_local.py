#!/usr/bin/env python3
"""Run explicit CPU/Metal model benchmarks; preserve measured runtime and resource limits."""
import argparse
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import threading
import time
from model_runtime import GpuSampler, hardware_snapshot, inspect_runtime, parse_runtime


def benchmark(bench, kind, model_dir, threads, rounds, output, device="cpu", gpu_sample_interval=1.0):
    output.mkdir(parents=True, exist_ok=True)
    env = dict(os.environ)
    settings = {key:str(threads) for key in ["RAYON_NUM_THREADS", "OMP_NUM_THREADS", "OPENBLAS_NUM_THREADS", "VECLIB_MAXIMUM_THREADS"]}
    env.update(settings)
    cmd = [str(bench.resolve()), kind, str(model_dir.resolve()), "--rounds", str(rounds), "--warmup", "1", "--device", device]
    cmd += ["--batch", "32"] if kind == "embed" else ["--docs", "30"]
    hardware = hardware_snapshot()
    sampler = GpuSampler(gpu_sample_interval if device != "cpu" else 0)
    sampler.start()
    started = time.monotonic()
    proc = subprocess.Popen(cmd, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
    streams = {}
    def collect(name, stream):
        streams[name] = stream.read()
        stream.close()
    readers = [threading.Thread(target=collect,args=("stdout",proc.stdout)), threading.Thread(target=collect,args=("stderr",proc.stderr))]
    for reader in readers: reader.start()
    # wait4 returns this process's measured resource usage directly. /usr/bin/time -l additionally
    # queries kern.clockrate on macOS, which is unavailable in some restricted environments.
    _, status, usage = os.wait4(proc.pid,0)
    proc.returncode = os.waitstatus_to_exitcode(status)
    for reader in readers: reader.join()
    wall = time.monotonic()-started
    gpu_resources = sampler.stop()
    stdout, stderr = streams["stdout"], streams["stderr"]
    prefix = f"threads-{threads}" if device == "cpu" else f"{device}-threads-{threads}"
    (output/f"{prefix}.stdout.log").write_text(stdout, encoding="utf-8")
    (output/f"{prefix}.stderr.log").write_text(stderr, encoding="utf-8")
    report = {"kind":kind,"modelDir":str(model_dir.resolve()),"threads":threads,"environment":settings,
        "rounds":rounds,"warmup":1,"command":cmd,"exitCode":proc.returncode,"wallSeconds":wall,
        "requestedDevice":device,"hardware":hardware,"gpuResources":gpu_resources}
    load = re.search(r"load ([\d.]+)s", stdout)
    runtime_payload = parse_runtime(stdout)
    runtime = inspect_runtime(runtime_payload, device)
    report.update({"runtime":runtime,"backend":runtime["backend"],"device":runtime["actualDevice"],
                   "dtype":runtime["dtype"],"runtimePayload":runtime_payload})
    if load: report["loadSeconds"] = float(load.group(1))
    report["peakRssBytes"] = usage.ru_maxrss*(1 if sys.platform == "darwin" else 1024)
    report["resourceSource"] = "wait4"
    report["userCpuSeconds"] = usage.ru_utime
    report["systemCpuSeconds"] = usage.ru_stime
    report["meanCpuCoresUsed"] = (usage.ru_utime + usage.ru_stime) / wall if wall > 0 else None
    report["memoryMeasurementNote"] = "wait4 ru_maxrss measures host process RSS; it does not include all Metal unified-memory allocations or provide a complete GPU allocation peak. GPU counters are system-wide snapshots, not a unique sum with RSS."
    if kind == "embed":
        metric = re.search(r"embed: ([\d.]+) s/batch mean.*?→ ([\d.]+) texts/s", stdout)
        if metric:
            report.update({"batchSize":32,"meanBatchSeconds":float(metric.group(1)),"textsPerSecond":float(metric.group(2))})
            report["targetMet"] = report["textsPerSecond"] >= 15
    else:
        metric = re.search(r"rerank 30 pairs: ([\d.]+) ms mean", stdout)
        if metric:
            report.update({"docs":30,"meanLatencyMs":float(metric.group(1))})
            report["targetMet"] = report["meanLatencyMs"] <= 3000
    report["ok"] = proc.returncode == 0 and metric is not None and runtime["verified"]
    report["targetMet"] = report["ok"] and report.get("targetMet",False)
    (output/f"{prefix}.json").write_text(json.dumps(report,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("kind",choices=["embed","rerank"])
    parser.add_argument("model_dir",type=Path)
    parser.add_argument("--bench",type=Path,required=True)
    parser.add_argument("--output",type=Path,required=True)
    parser.add_argument("--threads",type=int,nargs="+",default=[2,8])
    parser.add_argument("--rounds",type=int,default=3)
    parser.add_argument("--device",choices=["cpu","metal","auto"],default="cpu")
    parser.add_argument("--gpu-sample-interval",type=float,default=1.0,help="best-effort system-wide macOS counters; 0 disables")
    parser.add_argument("--cpu-baseline",type=Path,default=None,help="existing CPU summary.json or run.json; never overwritten")
    args=parser.parse_args()
    if args.rounds <= 0 or any(t <= 0 for t in args.threads): parser.error("rounds and threads must be positive")
    if args.gpu_sample_interval < 0: parser.error("GPU sampling interval cannot be negative")
    existing = args.output / "summary.json"
    if existing.exists():
        old = json.loads(existing.read_text())
        if old.get("requestedDevice", "cpu") != args.device:
            parser.error("output contains a different device baseline; choose a separate output directory")
    reports=[]
    for threads in args.threads:
        reports.append(benchmark(args.bench,args.kind,args.model_dir,threads,args.rounds,args.output,args.device,args.gpu_sample_interval))
    successful=[r for r in reports if r["ok"]]
    best=max(successful,key=lambda r:r["textsPerSecond"]) if args.kind=="embed" and successful else (
        min(successful,key=lambda r:r["meanLatencyMs"]) if successful else None)
    summary={"ok":len(successful)==len(reports),"kind":args.kind,"requestedDevice":args.device,"logicalCores":os.cpu_count(),"runs":reports,
        "recommendedThreads":best["threads"] if best else None,"targetMet":any(r.get("targetMet",False) for r in reports)}
    if args.cpu_baseline:
        baseline = json.loads(args.cpu_baseline.read_text())
        candidates = baseline.get("runs", [baseline])
        candidates = [r for r in candidates if r.get("ok") and r.get("kind") == args.kind
                      and Path(r.get("modelDir", "")).resolve() == args.model_dir.resolve()
                      and r.get("rounds") == args.rounds and r.get("warmup") == 1
                      and r.get("batchSize" if args.kind == "embed" else "docs") == (32 if args.kind == "embed" else 30)]
        metric_name = "textsPerSecond" if args.kind == "embed" else "meanLatencyMs"
        valid = [r for r in candidates if isinstance(r.get(metric_name), (int,float)) and r[metric_name] > 0]
        control = (max(valid,key=lambda r:r[metric_name]) if args.kind == "embed" else min(valid,key=lambda r:r[metric_name])) if valid else None
        summary["cpuBaseline"] = {"path":str(args.cpu_baseline.resolve()),"matchingWorkload":bool(control),"run":control,
                                 "runtimeVerified":bool(control and control.get("runtime",{}).get("verified"))}
        summary["speedupVsCpu"] = ((best[metric_name]/control[metric_name] if args.kind == "embed" else control[metric_name]/best[metric_name])
                                    if best and control else None)
        summary["ok"] &= bool(control)
    (args.output/"summary.json").write_text(json.dumps(summary,ensure_ascii=False,indent=2)+"\n",encoding="utf-8")
    print(json.dumps(summary,ensure_ascii=False))
    return 0 if summary["ok"] else 1


if __name__ == "__main__":
    raise SystemExit(main())
