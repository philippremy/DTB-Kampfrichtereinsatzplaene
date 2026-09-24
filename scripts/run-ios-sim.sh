#!/usr/bin/env bash
# Build the DEBUG app for the iOS simulator, boot an iPad simulator, open Simulator.app, install
# the app and launch it with its log streaming to this terminal. Ctrl-C stops the app.
#
#   scripts/run-ios-sim.sh                          # default device: iPad Air 11-inch (M4)
#   scripts/run-ios-sim.sh "iPad Pro 13-inch (M5)"  # any name from `xcrun simctl list devices`
#   DEVICE=<udid> scripts/run-ios-sim.sh            # or a UDID
#   NO_BUILD=1 scripts/run-ios-sim.sh               # reuse the last bundle in target/bundle/debug
#   FRESH=1 scripts/run-ios-sim.sh                  # wipe the app's data first (empty database)
#   SIMCTL_CHILD_DTB_KE_PERF_HUD=0 scripts/run-ios-sim.sh   # SIMCTL_CHILD_<VAR> reaches the app
#                                                           # as <VAR> (hides the FPS overlay)
set -euo pipefail
cd "$(dirname "$0")/.."

DEVICE="${1:-${DEVICE:-iPad Air 11-inch (M4)}}"
BUNDLE_ID="de.philippremy.DTB-Kampfrichtereinsatzplaene"
APP="target/bundle/debug/DTB Kampfrichtereinsatzpläne.app"

[[ "${NO_BUILD:-0}" == 1 ]] || cargo dtb-ke-bundle bundle --debug --target aarch64-apple-ios-sim
[[ -d "$APP" ]] || { echo "no bundle at $APP (run without NO_BUILD)" >&2; exit 1; }

# Resolve a name to a UDID (a UDID passes through). The simulator name may match several runtimes;
# take the first available one.
if [[ "$DEVICE" =~ ^[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}$ ]]; then
    UDID="$DEVICE"
else
    UDID="$(xcrun simctl list devices available \
        | grep -F "    $DEVICE (" | head -1 | grep -oE '[0-9A-F]{8}(-[0-9A-F]{4}){3}-[0-9A-F]{12}' || true)"
    [[ -n "$UDID" ]] || { echo "no available simulator named \"$DEVICE\"; try:" >&2
        xcrun simctl list devices available | grep -i ipad >&2; exit 1; }
fi

echo "==> simulator $DEVICE ($UDID)"
xcrun simctl boot "$UDID" 2>/dev/null || true   # already booted is fine
xcrun simctl bootstatus "$UDID" -b >/dev/null
# Show the device window: Xcode 27 replaced Simulator.app with Device Hub (inside Xcode.app); older
# Xcodes still have Simulator.app. Best-effort — the booted device runs the app either way. Device Hub
# takes no device argument that we know of, so pick the device in its sidebar.
XCODE_APPS="$(xcode-select -p)/../Applications"
if [[ -d "$XCODE_APPS/DeviceHub.app" ]]; then
    open "$XCODE_APPS/DeviceHub.app"
elif [[ -d "$(xcode-select -p)/Applications/Simulator.app" ]]; then
    open "$(xcode-select -p)/Applications/Simulator.app" --args -CurrentDeviceUDID "$UDID"
else
    echo "==> warning: neither Device Hub nor Simulator.app found; the app runs on the booted" >&2
    echo "    device ($UDID) but no window is opened for it." >&2
fi

if [[ "${FRESH:-0}" == 1 ]]; then
    xcrun simctl terminate "$UDID" "$BUNDLE_ID" 2>/dev/null || true
    xcrun simctl uninstall "$UDID" "$BUNDLE_ID" 2>/dev/null || true
fi

xcrun simctl install "$UDID" "$APP"
echo "==> launching $BUNDLE_ID — Ctrl-C to stop"
# --console-pty streams stdout/stderr (the app's log lines) and ties the app's lifetime to this
# process; a pipe into `head` would kill it, so leave the output unpiped.
exec xcrun simctl launch --console-pty --terminate-running-process "$UDID" "$BUNDLE_ID"
