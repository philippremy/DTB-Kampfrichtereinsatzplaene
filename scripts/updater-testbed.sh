#!/usr/bin/env bash
# Local testing rig for the in-app updater (see UPDATER.md for the real CI flow).
#
# Serves a manifest.json (+ optionally a real signed archive) from a throwaway
# HTTP server on localhost, so the running app can be pointed at it with
# DTB_KE_UPDATE_MANIFEST — no Codeberg release, no real keypair required for the
# default mode.
#
# Usage:
#   scripts/updater-testbed.sh [check|full] [options]
#
#   check (default)  Fastest. A throwaway 32-byte key (not a real signing key)
#                     + a hand-written manifest advertising a newer version.
#                     Exercises the check → toast → Skip/Later flow. Clicking
#                     "Installieren" will correctly FAIL (no real signature) —
#                     that's expected; check mode never builds an archive.
#
#   full              Needs the `zipsign` CLI (`cargo install zipsign`). Generates
#                     a real ed25519 keypair, builds + bundles the app (debug by
#                     default — pass --release for the real thing), packages +
#                     signs a real archive, and writes a manifest that points at
#                     it — exercises the full download → verify → install →
#                     relaunch path. macOS only for now (the archive layout
#                     mirrors what CI will produce, see UPDATER.md §2).
#
# Options:
#   --port <n>        HTTP port (default 8790)
#   --version <x.y.z> the "newer" version to advertise (default: current + 1 patch)
#   --release          (full mode) bundle the release build instead of debug
#
# Ctrl-C stops the server. Nothing here touches the committed assets/release.pub
# — the throwaway key lives under target/updater-testbed/ and is pointed to via
# DTB_KE_RELEASE_PUB, which build.rs reads in preference to assets/release.pub.

set -euo pipefail
cd "$(dirname "${BASH_SOURCE[0]}")/.."

MODE="${1:-check}"
if [[ "$MODE" == "--"* ]]; then MODE="check"; else shift || true; fi
PORT=8790
VERSION=""
BUNDLE_PROFILE="--debug"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --port) PORT="$2"; shift 2 ;;
    --version) VERSION="$2"; shift 2 ;;
    --release) BUNDLE_PROFILE="--release"; shift ;;
    *) echo "updater-testbed: unknown option $1" >&2; exit 2 ;;
  esac
done

BIN_NAME="DTB-Kampfrichtereinsatzpläne"
DISPLAY_NAME="DTB Kampfrichtereinsatzpläne"
CURRENT_VERSION="$(grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/')"
if [[ -z "$VERSION" ]]; then
  IFS='.' read -r maj min patch <<<"$CURRENT_VERSION"
  VERSION="${maj}.${min}.$((patch + 1))"
fi

BED="target/updater-testbed"
rm -rf "$BED"
mkdir -p "$BED"
echo "updater-testbed: mode=$MODE current=$CURRENT_VERSION advertising=$VERSION port=$PORT"

# The automatic check silently ignores a version the user already clicked
# "Überspringen" on — only a manual "Nach Updates suchen" bypasses that. Since
# this script advertises the same version (current + 1 patch) on every run
# unless told otherwise, a stale skip from an earlier test session is a classic
# "auto-check finds nothing, manual check works" trap. Best-effort, macOS only.
SETTINGS_TOML="$HOME/Library/Application Support/de.philippremy.DTB-Kampfrichtereinsatzpläne/Settings.toml"
if [[ -f "$SETTINGS_TOML" ]] && grep -q "^skipped_update = \"$VERSION\"" "$SETTINGS_TOML" 2>/dev/null; then
  echo
  echo "  ⚠ version $VERSION is already recorded as skipped in Settings.toml —"
  echo "    the AUTOMATIC check will silently ignore it (a manual 'Nach Updates"
  echo "    suchen' still finds it, which can look like the automatic path is"
  echo "    broken). Click 'Zurücksetzen' in Einstellungen → Aktualisierung, or"
  echo "    pass --version to advertise a different one."
  echo
fi

case "$MODE" in
  check)
    # A 32-byte file is all build.rs checks — it only needs to make
    # updater::available() true; check mode never calls run_install, so it is
    # never used to verify anything.
    python3 -c "import os; open('$BED/release.pub','wb').write(os.urandom(32))"

    cat > "$BED/manifest.json" <<EOF
{
  "schema": 1,
  "releases": [
    {
      "version": "$VERSION",
      "date": "$(date -u +%Y-%m-%d)",
      "notes_url": "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/tag/v$VERSION",
      "assets": [
        { "name": "$BIN_NAME-$VERSION-$(rustc -vV | sed -n 's/host: //p').tar.gz", "size": 35651584 }
      ]
    }
  ]
}
EOF

    echo "updater-testbed: building (DTB_KE_RELEASE_PUB=$BED/release.pub) …"
    DTB_KE_RELEASE_PUB="$PWD/$BED/release.pub" cargo build -p dtb-ke-ui
    ;;

  full)
    command -v zipsign >/dev/null || {
      echo "updater-testbed: 'zipsign' not found — cargo install zipsign" >&2
      exit 1
    }
    [[ "$(uname -s)" == "Darwin" ]] || {
      echo "updater-testbed: full mode is macOS-only for now (see UPDATER.md §2)" >&2
      exit 1
    }

    zipsign gen-key "$BED/release.priv" "$BED/release.pub" >/dev/null

    echo "updater-testbed: building + bundling ($BUNDLE_PROFILE) — this can take a while …"
    # No --formats: on macOS that means "just the .app", no .dmg (dmg only
    # builds when --formats is passed *and* names it — see bundle.rs).
    DTB_KE_RELEASE_PUB="$PWD/$BED/release.pub" cargo dtb-ke-bundle bundle "$BUNDLE_PROFILE"
    PROFILE_DIR=$([[ "$BUNDLE_PROFILE" == "--release" ]] && echo release || echo debug)
    APP_SRC="target/bundle/$PROFILE_DIR/${DISPLAY_NAME}.app"
    if [[ ! -d "$APP_SRC" ]]; then
      echo "updater-testbed: expected bundle at $APP_SRC — check 'cargo dtb-ke-bundle bundle' output above" >&2
      exit 1
    fi

    TARGET_TRIPLE="$(rustc -vV | sed -n 's/host: //p')"
    ARCHIVE_NAME="$BIN_NAME-$VERSION-$TARGET_TRIPLE.tar.gz"
    STAGE="$BED/stage"
    mkdir -p "$STAGE"
    # The archive's internal directory name must be $BIN_NAME.app — that's what
    # updater::run_install's bundle_path_in_archive() looks for, independent of
    # what dtb-ke-bundle names the on-disk .app (the display name).
    cp -R "$APP_SRC" "$STAGE/$BIN_NAME.app"
    tar -C "$STAGE" -czf "$BED/$ARCHIVE_NAME" "$BIN_NAME.app"
    rm -rf "$STAGE"

    echo "updater-testbed: signing …"
    zipsign sign tar "$BED/$ARCHIVE_NAME" "$BED/release.priv"

    echo "updater-testbed: writing manifest …"
    cargo dtb-ke-bundle manifest \
      --version "$VERSION" \
      --date "$(date -u +%Y-%m-%d)" \
      --notes-url "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/tag/v$VERSION" \
      --out "$BED/manifest.json" \
      "$BED/$ARCHIVE_NAME"
    ;;

  *)
    echo "updater-testbed: unknown mode '$MODE' (want 'check' or 'full')" >&2
    exit 2
    ;;
esac

# check mode always builds debug; full mode's binary matches $BUNDLE_PROFILE.
if [[ "$MODE" == "full" && "$BUNDLE_PROFILE" == "--release" ]]; then
  BIN_PATH="target/release/$BIN_NAME"
else
  BIN_PATH="target/debug/$BIN_NAME"
fi

echo
echo "updater-testbed: serving $BED on http://localhost:$PORT (Ctrl-C to stop)"
echo
echo "  In another terminal:"
echo "    DTB_KE_UPDATE_MANIFEST=http://localhost:$PORT/manifest.json ./$BIN_PATH"
echo
cd "$BED" && exec python3 -m http.server "$PORT"
