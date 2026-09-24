#!/usr/bin/env bash
# Idle CPU check: launches the app (debug by default) against an isolated, freshly seeded database (120
# competitions, the large one selected, nothing focused), lets it settle, and reports the process's
# CPU use over a window. Compare variants with the usual env switches, e.g.
#   scripts/perf-idle.sh
#   DTB_KE_VIEW_CACHE=0 scripts/perf-idle.sh   # without the view cache (on by default)
#   DTB_KE_PERF_HUD=0 scripts/perf-idle.sh      # the debug FPS HUD redraws every 500 ms while shown
#   DTB_KE_STRESS=empty scripts/perf-idle.sh    # nothing selected
# WINDOW=<secs> sets the measured window (default 10); PROFILE=release measures the release build.
set -euo pipefail
cd "$(dirname "$0")/.."
PROFILE="${PROFILE:-debug}"
cargo build -p dtb-ke-ui $([[ "$PROFILE" == release ]] && echo --release) 2>&1 | grep -E "^error" -A8 || true
BIN="${CARGO_TARGET_DIR:-target}/$PROFILE/dtb-ke-ui"
DATA="$(mktemp -d)"
WINDOW="${WINDOW:-10}"
cpu() { ps -o cputime= -p "$1" | awk -F: '{ if (NF==3) print $1*3600+$2*60+$3; else print $1*60+$2 }'; }
if [[ "${DTB_KE_STRESS:-}" == empty ]]; then unset DTB_KE_STRESS; else export DTB_KE_STRESS=idle; fi
DTB_KE_DATA_DIR="$DATA" "$BIN" >/dev/null 2>&1 &
PID=$!
sleep 10
C1="$(cpu $PID)"; sleep "$WINDOW"; C2="$(cpu $PID)"
kill $PID; rm -rf "$DATA"
echo "$C1 $C2 $WINDOW" | awk '{ printf "idle CPU over %d s: %.1f%% of one core\n", $3, ($2-$1)*100/$3 }'
