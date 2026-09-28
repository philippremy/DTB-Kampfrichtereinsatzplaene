#!/usr/bin/env python3
"""Build the DEBUG app for the iOS simulator, boot an iPad simulator, open its window, install the
app and launch it with its log streaming to this terminal. Ctrl-C stops the app.

Ports `scripts/run-ios-sim.sh`.

    run_ios_sim.py                          # default device: iPad Air 11-inch (M4)
    run_ios_sim.py "iPad Pro 13-inch (M5)"  # any name from `xcrun simctl list devices`
    DEVICE=<udid> run_ios_sim.py            # or a UDID
    NO_BUILD=1 run_ios_sim.py               # reuse the last bundle in target/bundle/debug
    FRESH=1 run_ios_sim.py                  # wipe the app's data first (empty database)
    SIMCTL_CHILD_DTB_KE_PERF_HUD=0 run_ios_sim.py   # SIMCTL_CHILD_<VAR> reaches the app as <VAR>
                                                     # (hides the FPS overlay)
"""

from __future__ import annotations

import os
import re
import subprocess
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
BUNDLE_ID = "de.philippremy.DTB-Kampfrichtereinsatzplaene"
IOS_TARGET = "aarch64-apple-ios-sim"
APP = Path(f"target/{IOS_TARGET}/debug/bundle/ios/DTB Kampfrichtereinsatzpläne.app")
UDID_RE = re.compile(r"^[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}$")


def main(argv: list[str]) -> int:
    os.chdir(WORKSPACE_ROOT)
    device = argv[1] if len(argv) > 1 else os.environ.get("DEVICE", "iPad Air 11-inch (M4)")

    if os.environ.get("NO_BUILD", "0") != "1":
        subprocess.run(
            ["cargo", "cargo-bundle", "-p", "dtb-ke-ui", "-f", "ios", "--target", IOS_TARGET],
            check=True,
        )
    if not APP.is_dir():
        print(f"no bundle at {APP} (run without NO_BUILD)", file=sys.stderr)
        return 1

    if UDID_RE.match(device):
        udid = device
    else:
        listing = subprocess.run(
            ["xcrun", "simctl", "list", "devices", "available"], capture_output=True, text=True
        ).stdout
        udid = None
        for line in listing.splitlines():
            if f"    {device} (" in line:
                m = re.search(r"[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}", line)
                if m:
                    udid = m.group(0)
                    break
        if udid is None:
            print(f'no available simulator named "{device}"; try:', file=sys.stderr)
            for line in listing.splitlines():
                if "ipad" in line.lower():
                    print(line, file=sys.stderr)
            return 1

    print(f"==> simulator {device} ({udid})")
    subprocess.run(["xcrun", "simctl", "boot", udid], stderr=subprocess.DEVNULL)  # already booted is fine
    subprocess.run(["xcrun", "simctl", "bootstatus", udid, "-b"], stdout=subprocess.DEVNULL, check=True)

    # Show the device window: Xcode 27 replaced Simulator.app with Device Hub (inside Xcode.app);
    # older Xcodes still have Simulator.app. Best-effort -- the booted device runs the app either
    # way. Device Hub takes no device argument that we know of, so pick the device in its sidebar.
    xcode_path = subprocess.run(["xcode-select", "-p"], capture_output=True, text=True).stdout.strip()
    xcode_apps = Path(xcode_path) / ".." / "Applications"
    device_hub = xcode_apps / "DeviceHub.app"
    simulator_app = Path(xcode_path) / "Applications" / "Simulator.app"
    if device_hub.is_dir():
        subprocess.run(["open", str(device_hub)])
    elif simulator_app.is_dir():
        subprocess.run(["open", str(simulator_app), "--args", "-CurrentDeviceUDID", udid])
    else:
        print(
            "==> warning: neither Device Hub nor Simulator.app found; the app runs on the booted\n"
            f"    device ({udid}) but no window is opened for it.",
            file=sys.stderr,
        )

    if os.environ.get("FRESH", "0") == "1":
        subprocess.run(["xcrun", "simctl", "terminate", udid, BUNDLE_ID], stderr=subprocess.DEVNULL)
        subprocess.run(["xcrun", "simctl", "uninstall", udid, BUNDLE_ID], stderr=subprocess.DEVNULL)

    subprocess.run(["xcrun", "simctl", "install", udid, str(APP)], check=True)
    print(f"==> launching {BUNDLE_ID} — Ctrl-C to stop")
    # --console-pty streams stdout/stderr (the app's log lines) and ties the app's lifetime to this
    # process; replacing this process (execvp, matching the original `exec`) rather than spawning a
    # child keeps Ctrl-C behavior identical -- a piped/waited subprocess would leave an extra layer
    # between the signal and the simulator process.
    os.execvp(
        "xcrun",
        ["xcrun", "simctl", "launch", "--console-pty", "--terminate-running-process", udid, BUNDLE_ID],
    )


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
