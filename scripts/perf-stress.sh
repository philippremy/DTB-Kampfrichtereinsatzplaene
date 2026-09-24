#!/usr/bin/env bash
# Perf baseline: builds the DEBUG binary, runs the in-app stress driver
# (crates/dtb-ke-ui/src/stress.rs — seeds 120 competitions into an isolated
# data dir, then scrolls the detail pane, scrolls the sidebar, and churns the
# selection, with no human input), and prints per-phase frame statistics.
#
#   scripts/perf-stress.sh                 # Time Profiler trace + frame stats
#   scripts/perf-stress.sh frames          # frame stats only (no Instruments)
#   scripts/perf-stress.sh trace "CPU Profiler"   # another Instruments template
#   SECS=40 scripts/perf-stress.sh         # longer run (default 24)
#   PROFILE=release scripts/perf-stress.sh # compare against an optimised build
#
# Output: target/perf/<stamp>/{run.trace,stress.log}. Open the .trace in
# Instruments.app. Uses `xctrace record` directly (what `cargo instruments`
# wraps) because it can pass --env to the launched app.
set -euo pipefail
cd "$(dirname "$0")/.."

MODE="${1:-trace}"
TEMPLATE="${2:-Time Profiler}"
SECS="${SECS:-24}"
PROFILE="${PROFILE:-debug}"

CARGO_ARGS=(build -p dtb-ke-ui)
[[ "$PROFILE" == release ]] && CARGO_ARGS+=(--release)
cargo "${CARGO_ARGS[@]}"

TARGET_DIR="${CARGO_TARGET_DIR:-target}"
BIN="$TARGET_DIR/$PROFILE/dtb-ke-ui"
[[ -x "$BIN" ]] || { echo "binary not found: $BIN" >&2; exit 1; }

STAMP="$(date +%Y%m%d-%H%M%S)-$PROFILE"
OUT="target/perf/$STAMP"
DATA="$OUT/data"
mkdir -p "$DATA"

export DTB_KE_STRESS=1 DTB_KE_STRESS_SECS="$SECS" DTB_KE_DATA_DIR="$PWD/$DATA"

if [[ "$MODE" == frames ]]; then
  "$BIN" || true
else
  xctrace record --template "$TEMPLATE" --output "$OUT/run.trace" \
    --time-limit "$((SECS + 25))s" --no-prompt \
    --env DTB_KE_STRESS=1 --env DTB_KE_STRESS_SECS="$SECS" \
    --env DTB_KE_DATA_DIR="$PWD/$DATA" \
    --launch -- "$BIN" || true
fi

LOG="$(ls -t "$DATA"/Logs/*.log 2>/dev/null | head -1 || true)"
if [[ -n "$LOG" ]]; then
  grep 'stress:' "$LOG" | tee "$OUT/stress.log"
else
  echo "no session log found under $DATA/Logs" >&2
fi
echo "results: $OUT"
