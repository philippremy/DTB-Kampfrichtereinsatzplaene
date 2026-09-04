#!/usr/bin/env bash
# Sets up the macOS Forgejo Actions runner (the 2017 16" MacBook Pro).
# Handles: Xcode Command Line Tools, Rust + both apple-darwin targets (for
# `bundle --universal`), Go (to build forgejo-runner from source — no macOS
# binary is published upstream), zipsign, notarization credential guidance,
# and the runner service (launchd).
#
# Idempotent: every step checks first, only prompts to install what's missing.
# Run it again any time; nothing here is destructive to existing state.
#
#   scripts/runner-setup-macos.sh [--yes]     --yes = don't prompt, just do it
#
# See RUNNERS.md for the full picture (signing/notarization secrets, why
# universal is built as two sequential slices + lipo, registration details).
#
# IMPORTANT — a launchd LaunchAgent, like a systemd service, does not source
# ~/.zprofile/~/.zshrc — so rustup's and Homebrew's PATH additions (normally
# written to a shell rc file) are invisible to it even though everything
# actually installed in this script targets this same user/$HOME correctly.
# The registration step below spells the daemon's PATH out explicitly in the
# plist's EnvironmentVariables for exactly that reason.

set -euo pipefail

ASSUME_YES=0
[[ "${1:-}" == "--yes" ]] && ASSUME_YES=1

confirm() {
  local prompt="$1"
  [[ "$ASSUME_YES" == "1" ]] && return 0
  read -r -p "$prompt [y/N] " reply
  [[ "$reply" =~ ^[Yy]$ ]]
}

step() { echo; echo "── $1 ──"; }
have() { command -v "$1" >/dev/null 2>&1; }

if [[ "$(uname -s)" != "Darwin" ]]; then
  echo "This script is for the macOS runner box only." >&2
  exit 1
fi

RUNNER_HOME="$HOME/forgejo-runner"
RUNNER_BIN="$RUNNER_HOME/forgejo-runner"

# ── 1. Xcode Command Line Tools ──────────────────────────────────────────
step "Xcode Command Line Tools"
if xcode-select -p >/dev/null 2>&1; then
  echo "already installed ($(xcode-select -p))"
elif confirm "Command Line Tools not found — trigger the installer? (a GUI dialog will pop up; accept it, then re-run this script)"; then
  xcode-select --install
  echo "Installer launched — finish it, then re-run this script."
  exit 0
fi

# ── 2. Homebrew (used only to fetch Go + coreutils below) ────────────────
step "Homebrew"
if ! have brew; then
  confirm "Homebrew not found — install it? (needed for Go and a couple of small CLI tools)" && \
    /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  # Apple Silicon vs. Intel prefix.
  if [[ -x /opt/homebrew/bin/brew ]]; then
    eval "$(/opt/homebrew/bin/brew shellenv)"
  elif [[ -x /usr/local/bin/brew ]]; then
    eval "$(/usr/local/bin/brew shellenv)"
  fi
else
  echo "already installed ($(brew --version | head -1))"
fi
# Captured for the launchd plist below — a LaunchAgent's PATH doesn't come
# from ~/.zprofile (where `brew shellenv` normally lives), so it has to be
# spelled out explicitly there instead of just relying on `have brew` above.
BREW_PREFIX="$(brew --prefix 2>/dev/null || echo /usr/local)"

# ── 3. Rust ───────────────────────────────────────────────────────────────
step "Rust toolchain (both apple-darwin targets, for --universal)"
if ! have rustup; then
  confirm "rustup not found — install it now (via the official install script)?" && \
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly
  source "$HOME/.cargo/env"
else
  echo "rustup already installed ($(rustup --version | head -1))"
fi
rustup toolchain install nightly >/dev/null
rustup target add x86_64-apple-darwin aarch64-apple-darwin --toolchain nightly

# ── 4. Go (to build forgejo-runner from source — no macOS binary ships) ──
step "Go (for building forgejo-runner)"
if have go; then
  echo "go already installed ($(go version))"
elif confirm "brew install go?"; then
  brew install go
fi

# ── 5. zipsign ────────────────────────────────────────────────────────────
step "zipsign (release-archive signing)"
if have zipsign; then
  echo "zipsign already installed"
elif confirm "cargo install zipsign?"; then
  cargo install zipsign
fi

# ── 6. WiX-equivalent: nothing needed — macOS packages its own .app/.dmg ──
# (hdiutil/codesign ship with the OS; nothing to install)

# ── 7. Code signing + notarization credentials ────────────────────────────
step "Code signing / notarization"
if security find-identity -v -p codesigning 2>/dev/null | grep -q "Developer ID Application"; then
  echo "a 'Developer ID Application' signing identity is present in the keychain"
else
  cat <<'EOF'
No "Developer ID Application" identity found in the keychain.

Tip builds (see tip.yml) sign ad-hoc and don't need this. Release builds
(release.yml) require it and will hard-fail without the MACOS_SIGN_IDENTITY
Codeberg secret. To set one up:
  1. On developer.apple.com, create/download a "Developer ID Application"
     certificate (needs a paid Apple Developer Program membership) and
     import it into this Mac's login keychain (double-click the .cer, or
     `security import cert.p12 -k ~/Library/Keychains/login.keychain-db`).
  2. Verify: security find-identity -v -p codesigning
  3. Set the Codeberg repo secrets:
       MACOS_SIGN_IDENTITY   — the "Developer ID Application: ..." string
       APPLE_API_KEY_ID, APPLE_API_ISSUER_ID, APPLE_API_KEY_P8
                             — an App Store Connect API key, for notarytool
         (developer.apple.com → Users and Access → Integrations → Keys)
See RUNNERS.md for the full list of secrets this project's CI needs.
EOF
fi

# ── 7b. CI cache directory (fixed target-dir for Tip's incremental builds) ─
# Forgejo checks each job out into a fresh, random per-job directory — see
# RUNNERS.md § target-dir caching. tip.yml points CARGO_TARGET_DIR at this
# fixed path so cargo's build output survives between runs instead of being
# reset by every job's fresh checkout. /Users/Shared is world-writable by
# default (macOS's shared folder), so no sudo/chown needed.
step "CI cache directory"
CI_CACHE_ROOT="/Users/Shared/Codeberg/DTB-Kampfrichtereinsatzplaene"
if [[ -d "$CI_CACHE_ROOT" ]]; then
  echo "already present: $CI_CACHE_ROOT"
elif confirm "Create $CI_CACHE_ROOT for tip.yml's persistent build cache?"; then
  mkdir -p "$CI_CACHE_ROOT"
fi

# ── 8. forgejo-runner (built from source) ─────────────────────────────────
step "forgejo-runner (source build)"
mkdir -p "$RUNNER_HOME"
if [[ -x "$RUNNER_BIN" ]]; then
  echo "forgejo-runner already built at $RUNNER_BIN"
elif confirm "Clone + build forgejo-runner from source (needs Go)?"; then
  src="$RUNNER_HOME/runner-src"
  if [[ ! -d "$src" ]]; then
    git clone --depth 1 https://code.forgejo.org/forgejo/runner.git "$src"
  fi
  (cd "$src" && go build -o "$RUNNER_BIN" .)
fi

# ── 9. registration + launchd service ─────────────────────────────────────
# Codeberg's "Create new Runner" page (repo → Settings → Actions → Runners)
# generates a UUID + token pair and shows the exact command to run — there is
# no separate `register` step any more: `forgejo-runner daemon` takes
# --url/--uuid/--token-url directly. The token goes in a file (not inline on
# the command line) — Codeberg's own example uses a file:// URL for exactly
# that reason. See RUNNERS.md § Registering a runner.
step "registration + launchd service"
TOKEN_FILE="$RUNNER_HOME/runner-token"
PLIST="$HOME/Library/LaunchAgents/de.philippremy.forgejo-runner.plist"
LABEL="macos-host:host"

if [[ -f "$TOKEN_FILE" && -f "$PLIST" ]]; then
  echo "already configured ($TOKEN_FILE + $PLIST exist) — delete both to reconfigure"
elif confirm "Configure + register this runner now? (you'll need the UUID + token Codeberg shows you when you create a new runner: repo → Settings → Actions → Runners → Create new Runner)"; then
  read -r -p "Instance URL [https://codeberg.org/]: " instance
  instance="${instance:-https://codeberg.org/}"
  read -r -p "Runner UUID: " uuid
  read -r -p "Runner token: " token

  echo -n "$token" > "$TOKEN_FILE"
  chmod 600 "$TOKEN_FILE"

  mkdir -p "$HOME/Library/LaunchAgents" "$RUNNER_HOME/logs"
  # See the IMPORTANT note at the top of this file — this PATH is the whole
  # reason EnvironmentVariables exists here at all. If you add another
  # per-user tool later (anything from `cargo install`/`brew install`), make
  # sure its bin directory is covered by one of these two prefixes, or add it
  # here explicitly.
  RUNNER_PATH="$HOME/.cargo/bin:$BREW_PREFIX/bin:$BREW_PREFIX/sbin:/usr/bin:/bin:/usr/sbin:/sbin"
  cat > "$PLIST" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>Label</key><string>de.philippremy.forgejo-runner</string>
  <key>ProgramArguments</key>
  <array>
    <string>$RUNNER_BIN</string>
    <string>daemon</string>
    <string>--url</string>
    <string>$instance</string>
    <string>--uuid</string>
    <string>$uuid</string>
    <string>--token-url</string>
    <string>file://$TOKEN_FILE</string>
    <string>--label</string>
    <string>$LABEL</string>
  </array>
  <key>EnvironmentVariables</key>
  <dict>
    <key>PATH</key><string>$RUNNER_PATH</string>
  </dict>
  <key>WorkingDirectory</key><string>$RUNNER_HOME</string>
  <key>RunAtLoad</key><true/>
  <key>KeepAlive</key><true/>
  <key>StandardOutPath</key><string>$RUNNER_HOME/logs/runner.log</string>
  <key>StandardErrorPath</key><string>$RUNNER_HOME/logs/runner.log</string>
</dict>
</plist>
EOF
  launchctl load "$PLIST"
  echo "loaded — check with: launchctl list | grep forgejo-runner"
  echo "(watch the first job run all the way through before trusting this is right — a"
  echo " missing PATH entry shows up as 'command not found' deep in a build step, not"
  echo " at startup.)"
  cat <<'EOF'

Note: this LaunchAgent runs while the user is logged in with an active
session. For a build box that should also survive being unattended at the
login screen, either enable automatic login for this user, or move the same
plist (with an absolute RUNNER_BIN path) to /Library/LaunchDaemons and load
it with sudo instead — a LaunchDaemon runs before any login but can't put up
GUI dialogs, which is fine since this runner never needs one.
EOF
fi

echo
echo "Done. Also make sure Energy Saver / Battery settings never let this Mac"
echo "sleep (System Settings → Lock Screen → disable auto-sleep, or"
echo "'sudo pmset -a sleep 0 disksleep 0'), or scheduled CI runs will just hang."
