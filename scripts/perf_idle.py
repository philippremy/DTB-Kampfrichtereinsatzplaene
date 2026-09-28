#!/usr/bin/env python3
"""Idle CPU check: launches the app (debug by default) against an isolated, freshly seeded database
(120 competitions, the large one selected, nothing focused), lets it settle, and reports the
process's CPU use over a window.

Ports `scripts/perf-idle.sh`. Compare variants with the usual env switches, e.g.:
    perf_idle.py
    DTB_KE_VIEW_CACHE=0 perf_idle.py   # without the view cache (on by default)
    DTB_KE_PERF_HUD=0 perf_idle.py     # the debug FPS HUD redraws every 500 ms while shown
    DTB_KE_STRESS=empty perf_idle.py   # nothing selected
WINDOW=<secs> sets the measured window (default 10); PROFILE=release measures the release build.
"""

from __future__ import annotations

import os
import re
import shutil
import signal
import subprocess
import sys
import tempfile
import time
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent


def cpu_seconds(pid: int) -> float:
    """`ps -o cputime=` is `[[hh:]mm:]ss` -- parse whichever shape it gives."""
    out = subprocess.run(["ps", "-o", "cputime=", "-p", str(pid)], capture_output=True, text=True)
    parts = [float(p) for p in out.stdout.strip().split(":")]
    if len(parts) == 3:
        h, m, s = parts
        return h * 3600 + m * 60 + s
    m, s = parts
    return m * 60 + s


def main() -> int:
    os.chdir(WORKSPACE_ROOT)
    profile = os.environ.get("PROFILE", "debug")

    cargo_args = ["cargo", "build", "-p", "dtb-ke-ui"]
    if profile == "release":
        cargo_args.append("--release")
    build = subprocess.run(cargo_args, capture_output=True, text=True)
    lines = (build.stdout + build.stderr).splitlines()
    for i, line in enumerate(lines):
        if line.startswith("error"):
            print("\n".join(lines[i : i + 9]))  # the line itself + up to 8 lines of context

    bin_path = Path(os.environ.get("CARGO_TARGET_DIR", "target")) / profile / "dtb-ke-ui"
    data_dir = Path(tempfile.mkdtemp())
    window = float(os.environ.get("WINDOW", "10"))

    env = os.environ.copy()
    env["DTB_KE_DATA_DIR"] = str(data_dir)
    if os.environ.get("DTB_KE_STRESS") == "empty":
        env.pop("DTB_KE_STRESS", None)
    else:
        env["DTB_KE_STRESS"] = "idle"

    proc = subprocess.Popen([str(bin_path)], env=env, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    try:
        time.sleep(10)
        c1 = cpu_seconds(proc.pid)
        time.sleep(window)
        c2 = cpu_seconds(proc.pid)
    finally:
        proc.send_signal(signal.SIGTERM)
        shutil.rmtree(data_dir, ignore_errors=True)

    print(f"idle CPU over {window:g} s: {(c2 - c1) * 100 / window:.1f}% of one core")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
