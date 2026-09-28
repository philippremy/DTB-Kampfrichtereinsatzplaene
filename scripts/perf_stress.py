#!/usr/bin/env python3
"""Perf baseline: builds the DEBUG binary, runs the in-app stress driver
(crates/dtb-ke-ui/src/stress.rs -- seeds 120 competitions into an isolated data dir, then scrolls the
detail pane, scrolls the sidebar, and churns the selection, with no human input), and prints
per-phase frame statistics.

Ports `scripts/perf-stress.sh`.

    perf_stress.py                          # Time Profiler trace + frame stats
    perf_stress.py frames                   # frame stats only (no Instruments)
    perf_stress.py trace "CPU Profiler"     # another Instruments template
    SECS=40 perf_stress.py                  # longer run (default 24)
    PROFILE=release perf_stress.py          # compare against an optimised build

Output: target/perf/<stamp>/{run.trace,stress.log}. Open the .trace in Instruments.app. Uses
`xctrace record` directly (what `cargo instruments` wraps) because it can pass --env to the
launched app.
"""

from __future__ import annotations

import datetime
import os
import subprocess
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent


def main(argv: list[str]) -> int:
    os.chdir(WORKSPACE_ROOT)

    mode = argv[1] if len(argv) > 1 else "trace"
    template = argv[2] if len(argv) > 2 else "Time Profiler"
    secs = int(os.environ.get("SECS", "24"))
    profile = os.environ.get("PROFILE", "debug")

    cargo_args = ["cargo", "build", "-p", "dtb-ke-ui"]
    if profile == "release":
        cargo_args.append("--release")
    subprocess.run(cargo_args, check=True)

    target_dir = Path(os.environ.get("CARGO_TARGET_DIR", "target"))
    bin_path = target_dir / profile / "dtb-ke-ui"
    if not os.access(bin_path, os.X_OK):
        print(f"binary not found: {bin_path}", file=sys.stderr)
        return 1

    stamp = f"{datetime.datetime.now():%Y%m%d-%H%M%S}-{profile}"
    out_dir = Path("target/perf") / stamp
    data_dir = out_dir / "data"
    data_dir.mkdir(parents=True, exist_ok=True)

    env = os.environ.copy()
    env["DTB_KE_STRESS"] = "1"
    env["DTB_KE_STRESS_SECS"] = str(secs)
    env["DTB_KE_DATA_DIR"] = str((WORKSPACE_ROOT / data_dir).resolve())

    if mode == "frames":
        subprocess.run([str(bin_path)], env=env)
    else:
        subprocess.run(
            [
                "xctrace", "record",
                "--template", template,
                "--output", str(out_dir / "run.trace"),
                "--time-limit", f"{secs + 25}s",
                "--no-prompt",
                "--env", f"DTB_KE_STRESS={env['DTB_KE_STRESS']}",
                "--env", f"DTB_KE_STRESS_SECS={env['DTB_KE_STRESS_SECS']}",
                "--env", f"DTB_KE_DATA_DIR={env['DTB_KE_DATA_DIR']}",
                "--launch", "--", str(bin_path),
            ]
        )

    logs_dir = data_dir / "Logs"
    log = None
    if logs_dir.is_dir():
        logs = sorted(logs_dir.glob("*.log"), key=lambda p: p.stat().st_mtime, reverse=True)
        log = logs[0] if logs else None

    if log is not None:
        stress_lines = [line for line in log.read_text(errors="replace").splitlines() if "stress:" in line]
        (out_dir / "stress.log").write_text("\n".join(stress_lines) + ("\n" if stress_lines else ""))
        print("\n".join(stress_lines))
    else:
        print(f"no session log found under {data_dir}/Logs", file=sys.stderr)

    print(f"results: {out_dir}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
