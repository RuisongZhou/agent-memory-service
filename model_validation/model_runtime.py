"""Runtime evidence and best-effort macOS GPU sampling; standard library only."""
from __future__ import annotations

import json
import os
import platform
import plistlib
import re
import subprocess
import threading
import time


def device_family(value):
    value = str(value or "").lower().strip()
    if value == "cpu" or value.startswith(("cpu:", "cpu(")):
        return "cpu"
    if value in ("metal", "mps") or value.startswith(("metal:", "metal(")):
        return "metal"
    return None


def inspect_runtime(payload, requested_device, *, require_f32=True):
    actual_device = payload.get("device")
    backend = payload.get("backend")
    dtype = payload.get("dtype")
    family = device_family(actual_device)
    issues = []
    if not family:
        issues.append("missing/unrecognized actual runtime device")
    elif requested_device != "auto" and family != requested_device:
        issues.append(f"requested {requested_device} but actual runtime is {family}")
    if not isinstance(backend, str) or not backend.strip():
        issues.append("missing actual runtime backend")
    if family == "metal" and isinstance(backend, str) and not any(s in backend.lower() for s in ("metal", "mps")):
        issues.append("Metal device without a Metal/MPS backend label")
    if require_f32 and str(dtype or "").lower() not in ("float32", "f32", "fp32"):
        issues.append("float32 runtime evidence required")
    return {"verified": not issues, "requestedDevice": requested_device, "actualDevice": actual_device,
            "deviceFamily": family, "backend": backend, "dtype": dtype, "issues": issues,
            "metalVerified": not issues and family == "metal"}


def parse_runtime(stdout):
    # The bench's preferred contract is one 'runtime { ... }' JSON line.
    for line in stdout.splitlines():
        match = re.match(r"\s*runtime\s*:?\s*(\{.*\})\s*$", line, re.IGNORECASE)
        if match:
            try:
                value = json.loads(match.group(1))
                if isinstance(value, dict):
                    return value
            except json.JSONDecodeError:
                pass
    values = {}
    for key in ("backend", "device", "dtype"):
        match = re.search(rf"\b{key}\s+([^|\s]+)", stdout, re.IGNORECASE)
        if match:
            values[key] = match.group(1)
    return values


def gpu_snapshot():
    if platform.system() != "Darwin":
        return {"available": False, "reason": "IOAccelerator sampling is macOS-only"}
    try:
        result = subprocess.run(["/usr/sbin/ioreg", "-r", "-c", "IOAccelerator", "-a"],
                                capture_output=True, timeout=5, check=True)
        roots = plistlib.loads(result.stdout)
        devices = []
        def visit(node):
            if not isinstance(node, dict):
                return
            stats = node.get("PerformanceStatistics")
            if isinstance(stats, dict):
                devices.append({"name": node.get("IORegistryEntryName") or node.get("IOClass"),
                                "performanceStatistics": {k: v for k, v in stats.items()
                                                          if isinstance(v, (int, float, str, bool))}})
            for child in node.get("IORegistryEntryChildren", []):
                visit(child)
        for root in roots:
            visit(root)
        return {"available": bool(devices), "devices": devices,
                "source": "ioreg IOAccelerator PerformanceStatistics", "scope": "system-wide snapshots"}
    except (OSError, subprocess.SubprocessError, ValueError, TypeError) as error:
        return {"available": False, "reason": f"{type(error).__name__}: {error}"}


def hardware_snapshot():
    result = {"system": platform.system(), "machine": platform.machine(), "release": platform.release(),
              "logicalCores": os.cpu_count()}
    if platform.system() == "Darwin":
        try:
            value = subprocess.run(["/usr/sbin/sysctl", "-n", "hw.memsize"], capture_output=True,
                                   text=True, timeout=5, check=True)
            result["physicalMemoryBytes"] = int(value.stdout.strip())
        except (OSError, subprocess.SubprocessError, ValueError):
            result["physicalMemoryBytes"] = None
    return result


class GpuSampler:
    def __init__(self, interval=1.0):
        self.interval = interval
        self.samples = []
        self._stop = threading.Event()
        self._thread = None
    def _loop(self):
        started = time.monotonic()
        while not self._stop.is_set():
            self.samples.append({"elapsedSeconds": time.monotonic() - started, **gpu_snapshot()})
            self._stop.wait(self.interval)
    def start(self):
        if self.interval > 0:
            self._thread = threading.Thread(target=self._loop, daemon=True)
            self._thread.start()
    def stop(self):
        self._stop.set()
        if self._thread:
            self._thread.join(timeout=6)
        return {"intervalSeconds": self.interval, "samples": self.samples,
                "scope": "system-wide; may include other applications and miss short bursts",
                "processGpuMemoryPeakBytes": None,
                "memoryNote": "wait4 RSS does not cover all Metal unified-memory allocations. IOAccelerator counters, if exposed, are not a complete process GPU peak. Do not sum them with RSS as unique total memory."}
