#!/usr/bin/env bash
# Sets up the Linux Forgejo Actions runner (the CachyOS / Arch-based box).
# Handles: x86_64 (native) + aarch64 (cross, via an extracted Debian sysroot —
# no QEMU) Rust builds for -gnu targets, x86_64 + aarch64 musl cross
# toolchains + Alpine-sourced static sysroots for -musl targets, Linux
# packaging tools (.deb/.rpm/.AppImage), zipsign, and the forgejo-runner
# binary + service.
#
# The Windows VM this box hosts is a SEPARATE concern — see
# scripts/provision-windows-vm.sh.
#
# Idempotent: every step checks first, only prompts to install what's missing.
# Run it again any time; nothing here is destructive to existing state.
#
#   scripts/runner-setup-linux.sh [--yes]     --yes = don't prompt, just do it
#
# See RUNNERS.md for the full picture (why aarch64 is cross-compiled this way,
# what each installed thing is for, how to re-register if you ever need to).
#
# IMPORTANT — everything the *daemon* needs at build time (rustup/cargo,
# zipsign) is installed for the 'runner' system user specifically, not
# whoever happens to be running this script. A systemd service has no shell,
# sources no ~/.bashrc or ~/.profile, and gets none of the PATH additions an
# interactive login would — a plain `curl … | sh` rustup install under your
# own account would leave the daemon with literally no concept of where Rust
# (or anything else installed the same way) lives. See the systemd step below
# for the other half of the fix (an explicit Environment=PATH=…).

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
# Existence check scoped to the 'runner' account's own install, not whatever
# is (or isn't) on the invoking user's PATH.
have_as_runner() { sudo -u runner -H bash -c "command -v '$1'" >/dev/null 2>&1; }

if ! have pacman; then
  echo "This script assumes an Arch-based host (pacman). Adapt it for anything else." >&2
  exit 1
fi

SYSROOT=/opt/sysroots/aarch64-linux-gnu
RUNNER_BIN=/usr/local/bin/forgejo-runner
RUNNER_HOME=/home/runner
CARGO_BIN="$RUNNER_HOME/.cargo/bin"
STAGE_DIR="$(mktemp -d)"
trap 'rm -rf "$STAGE_DIR"' EXIT

# ── 1. runner user (created first — everything per-user below targets it) ─
step "runner user"
if ! id runner >/dev/null 2>&1; then
  confirm "Create the 'runner' system user ($RUNNER_HOME)?" && \
    sudo useradd --create-home --shell /bin/bash runner
else
  echo "'runner' already exists"
fi

# ── 2. Rust (installed for 'runner', not the invoking user — see the note
#      at the top of this file) ────────────────────────────────────────────
step "Rust toolchain (for the 'runner' user)"
if have_as_runner rustup; then
  echo "rustup already installed for runner"
elif confirm "rustup not found for 'runner' — install it now (via the official install script)?"; then
  sudo -u runner -H bash -c \
    'curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain nightly'
fi
sudo -u runner -H "$CARGO_BIN/rustup" toolchain install nightly >/dev/null
sudo -u runner -H "$CARGO_BIN/rustup" target add aarch64-unknown-linux-gnu --toolchain nightly
sudo -u runner -H "$CARGO_BIN/rustup" target add x86_64-unknown-linux-musl aarch64-unknown-linux-musl --toolchain nightly

# ── 3. aarch64 cross toolchain ───────────────────────────────────────────
# A system package (pacman → /usr/bin) — every user, including 'runner',
# already sees this on PATH with no extra wiring.
step "aarch64 cross toolchain"
if ! have aarch64-linux-gnu-gcc; then
  confirm "aarch64-linux-gnu-gcc not found — install aarch64-linux-gnu-gcc/binutils/glibc via pacman?" && \
    sudo pacman -S --needed aarch64-linux-gnu-gcc aarch64-linux-gnu-binutils aarch64-linux-gnu-glibc
else
  echo "aarch64-linux-gnu-gcc already installed"
fi

# ── 4. aarch64 sysroot (fontconfig/freetype/wayland/EGL — see RUNNERS.md) ─
step "aarch64 sysroot ($SYSROOT)"
if [[ -d "$SYSROOT/usr/lib/aarch64-linux-gnu" ]]; then
  echo "sysroot already assembled — delete $SYSROOT to force a rebuild"
elif confirm "Assemble the aarch64 sysroot from Debian's package pool (no execution, just unpacked .debs)?"; then
  if ! have dpkg-deb; then
    echo "dpkg-deb not found — install the 'dpkg' pacman package first." >&2
    exit 1
  fi
  sudo mkdir -p "$SYSROOT"
  sudo chown "$(id -u):$(id -g)" "$SYSROOT"

  DEBIAN_SUITE=bookworm
  DEB_BASE="https://ftp.debian.org/debian"
  INDEX="$STAGE_DIR/Packages"
  echo "  fetching the $DEBIAN_SUITE arm64 package index …"
  curl -fsSL "$DEB_BASE/dists/$DEBIAN_SUITE/main/binary-arm64/Packages.xz" -o "$INDEX.xz"
  xz -dc "$INDEX.xz" > "$INDEX"

  # Looked up by name, not hardcoded — exact filenames/versions drift with
  # every Debian point release, so resolving them here (rather than pinning a
  # snapshot in this script) is what keeps this correct indefinitely. Pinned
  # to what's actually needed (see the cargo tree audit in RUNNERS.md):
  # fontconfig + freetype (font-kit), wayland-client (gpui_linux), libegl
  # (wgpu-hal/khronos-egl), xcb + xkbcommon (+x11) (gpui_linux's X11 backend,
  # missed by the first pass of the audit — only surfaced once a real link
  # actually got this far) — plus their own transitive C deps.
  #
  # NOTE: libfontconfig-dev / libfreetype-dev, not libfontconfig1-dev /
  # libfreetype6-dev — verified against the real .deb contents (not
  # guessed): the *1/*6-suffixed packages are empty Debian transitional
  # packages (changelog + copyright only, no headers, no .so symlink at
  # all); the unsuffixed ones are what actually ships
  # usr/lib/aarch64-linux-gnu/libfontconfig.so / libfreetype.so, which the
  # linker needs for a bare -lfontconfig/-lfreetype.
  # The direct set above isn't enough on its own: each of those .so files
  # has its own further DT_NEEDED chain (e.g. libfontconfig.so needs expat,
  # libxcb.so needs libXau/libXdmcp, libxkbcommon-x11.so needs libxcb-xkb,
  # libEGL.so needs the Mesa/DRI/GBM stack) — the linker still fails on
  # "undefined reference" for those even once -rpath-link (below) lets it
  # search for them. Rather than keep discovering these one broken link at
  # a time, this is the *complete* transitive runtime closure — resolved
  # programmatically from the real Depends: fields in the live Debian
  # index, not hand-curated (a Python BFS over Packages, first alternative
  # of each Depends taken, restricted to lib* packages, excluding the ones
  # that come from the host's own glibc/gcc — libc6/libgcc-s1/libstdc++6).
  PACKAGES=(
    libfontconfig1 libfontconfig-dev
    libfreetype6 libfreetype-dev
    libwayland-client0 libwayland-dev
    libwayland-server0
    libegl1 libegl-dev
    libegl-mesa0
    libxcb1 libxcb1-dev
    libxkbcommon0 libxkbcommon-dev
    libxkbcommon-x11-0 libxkbcommon-x11-dev
    libexpat1 libexpat1-dev
    libpng16-16
    zlib1g
    libffi8
    libbrotli1
    libbsd0
    libdrm2
    libdrm-common
    libgbm1
    libglapi-mesa
    libglvnd0
    libmd0
    libx11-6
    libx11-data
    libx11-xcb1
    libxau6
    libxcb-dri2-0
    libxcb-dri3-0
    libxcb-present0
    libxcb-randr0
    libxcb-sync1
    libxcb-xfixes0
    libxcb-xkb1
    libxdmcp6
    libxshmfence1
  )
  resolve_deb() {
    awk -v pkg="$1" '
      BEGIN { RS=""; FS="\n" }
      {
        name = ""; file = "";
        for (i = 1; i <= NF; i++) {
          if ($i ~ /^Package: /)  { name = substr($i, 10) }
          if ($i ~ /^Filename: /) { file = substr($i, 11) }
        }
        if (name == pkg && file != "") { print file; exit }
      }' "$INDEX"
  }
  for pkg in "${PACKAGES[@]}"; do
    rel="$(resolve_deb "$pkg")"
    if [[ -z "$rel" ]]; then
      echo "  ! could not find '$pkg' in the $DEBIAN_SUITE arm64 index — skipping" >&2
      continue
    fi
    deb="$STAGE_DIR/$(basename "$rel")"
    echo "  fetching $(basename "$rel")"
    curl -fsSL "$DEB_BASE/$rel" -o "$deb"
    dpkg-deb -x "$deb" "$SYSROOT"
  done
  echo "sysroot assembled at $SYSROOT"
fi

# ── 5. musl cross toolchains (x86_64 + aarch64) ──────────────────────────
# Arch's own `musl` package only ships a C compiler (musl-gcc) for the native
# arch, no C++ (`x86_64-linux-musl-g++` doesn't exist anywhere on a stock Arch
# system) and nothing at all for aarch64 — confirmed the hard way against a
# real build (cc-rs: "failed to find tool 'x86_64-linux-musl-g++'"). If you
# `pacman -S musl`'d your way past that earlier, remove it now
# (`sudo pacman -R musl`) — it's redundant with (and can shadow/conflict
# with) the toolchain this step installs.
#
# musl.cc's prebuilt cross toolchains provide a complete gcc+g+++musl-libc+
# libstdc++ set for both architectures, under exactly the <arch>-linux-musl-*
# naming rustc's own *-unknown-linux-musl target specs already look for
# (confirmed against that same real error) — just putting bin/ on PATH is
# enough, no CC_<target>/CXX_<target> env vars needed. They also default to
# a generic, non-tuned baseline for their arch (no CachyOS-style x86-64-v3/v4
# codegen) — see the note in this turn's reply for why that matters for
# anything a -sys crate's build.rs compiles from source (ring, aws-lc-sys,
# turso_core, sysinfo, …), not just the libraries step 6 below vendors.
step "musl cross toolchains (x86_64 + aarch64)"
for arch in x86_64 aarch64; do
  cross_dir="/opt/${arch}-linux-musl-cross"
  if [[ -x "$cross_dir/bin/${arch}-linux-musl-gcc" ]]; then
    echo "$arch-linux-musl-cross already installed at $cross_dir"
  elif confirm "Download + extract musl.cc's ${arch}-linux-musl-cross toolchain to $cross_dir?"; then
    tarball="$STAGE_DIR/${arch}-linux-musl-cross.tgz"
    echo "  fetching ${arch}-linux-musl-cross.tgz …"
    curl -fsSL "https://musl.cc/${arch}-linux-musl-cross.tgz" -o "$tarball"
    sudo mkdir -p /opt
    sudo tar -xzf "$tarball" -C /opt
    echo "  installed at $cross_dir"
  fi
  # Interactive login shells (manual test builds) get it via profile.d; the
  # 'runner' systemd service gets it via the unit's own explicit PATH= in
  # step 12 below — same two-audiences split as the PATH note at the top of
  # this file.
  profile="/etc/profile.d/${arch}-linux-musl-cross.sh"
  if [[ ! -f "$profile" ]]; then
    echo "export PATH=\"$cross_dir/bin:\$PATH\"" | sudo tee "$profile" >/dev/null
  fi
done

# ── 6. musl sysroots (Alpine, x86_64 + aarch64) ──────────────────────────
# Same underlying problem as the aarch64-gnu sysroot above (fontconfig/
# freetype/wayland/EGL/xcb/xkbcommon aren't part of any base musl toolchain),
# same fix (unpack real prebuilt packages, never execute foreign-arch code)
# — Alpine's package pool instead of Debian's, because the musl target links
# these STATICALLY by default (`crt-static` is on for every
# *-unknown-linux-musl target — confirmed the hard way: `/usr/bin/ld: cannot
# find -lxcb ... have you installed the static version?`), and Arch ships
# none of these as .a anywhere. Alpine's own -dev packages are built against
# musl natively and routinely carry the .a alongside the .so — and, same as
# the toolchain above, Alpine targets a generic/portable baseline rather
# than anything CachyOS-style microarchitecture-tuned.
#
# Alpine .apk files are themselves just (concatenated) gzip'd tars — `tar
# -xzf` extracts them directly, same "no execution of foreign-arch code"
# guarantee as `dpkg-deb -x` above; no apk/abuild tooling needed.
#
# UNVERIFIED against a real Alpine mirror / a real musl link from this box
# yet — this package list is a first pass at the same functional set the
# aarch64-gnu sysroot above needed (fontconfig, freetype, wayland, EGL/mesa,
# xcb, xkbcommon(+x11), expat, png, zlib, ffi, brotli, bsd, drm, X11/Xau/
# Xdmcp/Xshmfence), translated to Alpine's package names — several listed
# with a speculative *-static candidate alongside *-dev, since Alpine splits
# the static archive into its own package for some libraries and bundles it
# into *-dev for others. Same resolve-by-name/skip-if-missing tolerance as
# the Debian loop above, so a wrong guess here is skipped, not fatal. Expect
# to extend this list exactly the way the aarch64-gnu one was built up: run
# a real `cargo build --target <arch>-unknown-linux-musl`, feed whatever
# `cannot find -lXXX` names back into ALPINE_PACKAGES below.
step "musl sysroots (Alpine)"
ALPINE_BASE="https://dl-cdn.alpinelinux.org/alpine/latest-stable"
ALPINE_PACKAGES=(
  fontconfig fontconfig-dev fontconfig-static
  freetype freetype-dev freetype-static
  wayland wayland-dev wayland-libs-client wayland-libs-server
  mesa mesa-dev mesa-egl mesa-gbm mesa-gl
  libglvnd libglvnd-dev
  libxcb libxcb-dev libxcb-static
  libxkbcommon libxkbcommon-dev
  libxkbcommon-x11
  expat expat-dev expat-static
  libpng libpng-dev libpng-static
  zlib zlib-dev zlib-static
  libffi libffi-dev
  brotli-libs brotli-dev brotli-static
  libbsd libbsd-dev
  libdrm libdrm-dev
  libx11 libx11-dev libx11-static
  libxau libxau-dev
  libxdmcp libxdmcp-dev
  libxshmfence libxshmfence-dev
)
for arch in x86_64 aarch64; do
  musl_sysroot="/opt/sysroots/${arch}-linux-musl"
  if [[ -d "$musl_sysroot/usr/lib" ]]; then
    echo "$arch musl sysroot already assembled — delete $musl_sysroot to force a rebuild"
    continue
  fi
  confirm "Assemble the $arch musl sysroot from Alpine's package pool (no execution, just unpacked .apks)?" || continue

  sudo mkdir -p "$musl_sysroot"
  sudo chown "$(id -u):$(id -g)" "$musl_sysroot"

  apk_index="$STAGE_DIR/APKINDEX-$arch"
  : > "$apk_index"
  for repo in main community; do
    echo "  fetching Alpine $repo/$arch package index …"
    idx_tar="$STAGE_DIR/APKINDEX-$repo-$arch.tar.gz"
    if curl -fsSL "$ALPINE_BASE/$repo/$arch/APKINDEX.tar.gz" -o "$idx_tar"; then
      tar -xzf "$idx_tar" -O APKINDEX >> "$apk_index" 2>/dev/null || true
      echo >> "$apk_index"  # blank-line separator between repos for the awk paragraph parser below
    fi
  done

  resolve_apk() {
    # Alpine apk filenames are deterministic (<name>-<version>.apk, no
    # separate "Filename:" field like Debian's index) — P:/V: are enough.
    awk -v pkg="$1" '
      BEGIN { RS=""; FS="\n" }
      {
        name=""; ver="";
        for (i = 1; i <= NF; i++) {
          if ($i ~ /^P:/) { name = substr($i, 3) }
          if ($i ~ /^V:/) { ver = substr($i, 3) }
        }
        if (name == pkg && ver != "") { print ver; exit }
      }' "$apk_index"
  }
  for pkg in "${ALPINE_PACKAGES[@]}"; do
    ver="$(resolve_apk "$pkg")"
    if [[ -z "$ver" ]]; then
      echo "  ! could not find '$pkg' in the Alpine $arch index — skipping" >&2
      continue
    fi
    fname="${pkg}-${ver}.apk"
    apk_file="$STAGE_DIR/$fname"
    downloaded=0
    for repo in main community; do
      if curl -fsSL "$ALPINE_BASE/$repo/$arch/$fname" -o "$apk_file" 2>/dev/null; then
        downloaded=1
        break
      fi
    done
    if [[ "$downloaded" != "1" ]]; then
      echo "  ! could not download $fname from main or community — skipping" >&2
      continue
    fi
    echo "  fetching $fname"
    tar -xzf "$apk_file" -C "$musl_sysroot" 2>/dev/null || \
      echo "  ! $fname didn't extract cleanly (unexpected apk format?) — skipping" >&2
  done
  echo "$arch musl sysroot assembled at $musl_sysroot"
done

# ── 7. Linux packaging tools ─────────────────────────────────────────────
# Also all system packages — no per-user PATH concern.
step "Linux packaging tools (.deb / .rpm / .AppImage)"
NEEDED_PKGS=()
have rpmbuild || NEEDED_PKGS+=(rpm-tools)
have appimagetool || NEEDED_PKGS+=(appimagetool) # AUR on plain Arch; present on CachyOS's extra repos in some setups
have fusermount || NEEDED_PKGS+=(fuse2)          # AppImage runtime needs FUSE
have md5sum || NEEDED_PKGS+=(coreutils)
if [[ ${#NEEDED_PKGS[@]} -gt 0 ]]; then
  echo "missing: ${NEEDED_PKGS[*]}"
  if confirm "Install via pacman (falls back to a note for anything AUR-only)?"; then
    sudo pacman -S --needed "${NEEDED_PKGS[@]}" 2>&1 | tee /tmp/pacman-pkg.log || {
      echo "Some packages may be AUR-only (e.g. appimagetool) — install with your AUR helper" \
           "(paru -S appimagetool) if pacman couldn't find them." >&2
    }
  fi
else
  echo "all present"
fi

# ── 8. zipsign ────────────────────────────────────────────────────────────
# Also for 'runner' — `cargo install` puts it in $CARGO_BIN, same reasoning
# as step 2.
step "zipsign (release-archive signing, for the 'runner' user)"
if have_as_runner zipsign; then
  echo "zipsign already installed for runner"
elif confirm "cargo install zipsign (as 'runner')?"; then
  sudo -u runner -H "$CARGO_BIN/cargo" install zipsign
fi

# ── 9. libvirt/QEMU (hosts the Windows VM — see provision-windows-vm.sh) ──
# This one deliberately targets the INVOKING user, not 'runner' — you manage
# the Windows VM interactively (virsh, provision-windows-vm.sh), the daemon
# never touches libvirt itself.
step "libvirt / QEMU (for the Windows VM)"
if have virsh; then
  echo "libvirt already installed"
elif confirm "Install qemu-full, libvirt, virt-install, edk2-ovmf (UEFI firmware, needed for a modern Windows guest)?"; then
  sudo pacman -S --needed qemu-full libvirt virt-install edk2-ovmf dnsmasq
  sudo systemctl enable --now libvirtd
  sudo usermod -aG libvirt "$USER"
  echo "Added $USER to the libvirt group — log out/in (or 'newgrp libvirt') before running provision-windows-vm.sh."
fi

# ── 10. forgejo-runner binary ─────────────────────────────────────────────
# System-wide (/usr/local/bin) — no per-user PATH concern.
step "forgejo-runner binary"
if have forgejo-runner; then
  echo "forgejo-runner already installed ($(forgejo-runner --version 2>&1 | head -1))"
elif confirm "Download the prebuilt forgejo-runner linux/amd64 binary?"; then
  VER="$(curl -fsSL https://data.forgejo.org/api/v1/repos/forgejo/runner/releases/latest | grep -o '"name":"v[^"]*"' | head -1 | sed 's/"name":"v//;s/"//')"
  curl -fsSL -o /tmp/forgejo-runner \
    "https://code.forgejo.org/forgejo/runner/releases/download/v${VER}/forgejo-runner-${VER}-linux-amd64"
  chmod +x /tmp/forgejo-runner
  sudo mv /tmp/forgejo-runner "$RUNNER_BIN"
  echo "installed forgejo-runner v$VER"
fi

# ── 11. CI cache directory (fixed target-dir for Tip's incremental builds) ─
# Forgejo checks each job out into a fresh, random per-job directory (see
# RUNNERS.md § target-dir caching) — without a pre-existing, fixed absolute
# CARGO_TARGET_DIR outside that ephemeral checkout, tip.yml's incremental
# caching would silently do nothing every run. Owned by 'runner' (the user
# the daemon/service runs as) so cargo can create subdirectories under it
# without sudo mid-build.
step "CI cache directory"
CI_CACHE_ROOT="/var/cache/Codeberg/DTB-Kampfrichtereinsatzplaene"
if [[ -d "$CI_CACHE_ROOT" ]]; then
  echo "already present: $CI_CACHE_ROOT"
elif confirm "Create $CI_CACHE_ROOT (owned by 'runner') for tip.yml's persistent build cache?"; then
  sudo mkdir -p "$CI_CACHE_ROOT"
  sudo chown -R runner:runner "$CI_CACHE_ROOT"
fi

# ── 12. registration + systemd service ────────────────────────────────────
# Codeberg's "Create new Runner" page (repo → Settings → Actions → Runners)
# generates a UUID + token pair and shows the exact command to run — there is
# no separate `register` step any more: `forgejo-runner daemon` takes
# --url/--uuid/--token-url directly. The token goes in a file (not inline on
# the command line) — Codeberg's own example uses a file:// URL for exactly
# that reason. See RUNNERS.md § Registering a runner (incl. why a token is
# single-use — get a fresh one if a previous attempt here ever failed).
step "registration + systemd service"
TOKEN_FILE="$RUNNER_HOME/runner-token"
UNIT=/etc/systemd/system/forgejo-runner.service
LABEL="linux-host:host"

if [[ -f "$TOKEN_FILE" && -f "$UNIT" ]]; then
  echo "already configured ($TOKEN_FILE + $UNIT exist) — delete both to reconfigure"
elif confirm "Configure + register this runner now? (you'll need the UUID + token Codeberg shows you when you create a new runner: repo → Settings → Actions → Runners → Create new Runner)"; then
  read -r -p "Instance URL [https://codeberg.org/]: " instance
  instance="${instance:-https://codeberg.org/}"
  read -r -p "Runner UUID: " uuid
  read -r -p "Runner token: " token

  echo -n "$token" | sudo -u runner tee "$TOKEN_FILE" >/dev/null
  sudo chmod 600 "$TOKEN_FILE"
  sudo chown runner:runner "$TOKEN_FILE"

  # The whole point of this unit: everything the daemon (and every job it
  # spawns) needs is spelled out explicitly here, because systemd gives a
  # service none of the environment an interactive shell would build up
  # (~/.bashrc, ~/.profile, ~/.cargo/env are never sourced). Without the
  # explicit PATH below, 'cargo'/'rustc'/'zipsign' are literally not found —
  # this is the actual bug that prompted this rewrite. If you add another
  # per-user tool later (anything installed via `cargo install`, `rustup
  # component add`, or similar), it needs the same treatment: install it as
  # 'runner' and make sure its bin directory ends up in this PATH.
  sudo tee "$UNIT" >/dev/null <<EOF
[Unit]
Description=Forgejo Actions runner
After=network.target

[Service]
User=runner
WorkingDirectory=$RUNNER_HOME
Environment="PATH=$CARGO_BIN:/opt/x86_64-linux-musl-cross/bin:/opt/aarch64-linux-musl-cross/bin:/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
ExecStart=$RUNNER_BIN daemon --url $instance --uuid $uuid --token-url file://$TOKEN_FILE --label $LABEL
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
EOF
  sudo systemctl daemon-reload
  sudo systemctl enable --now forgejo-runner.service
  echo "started — check with: journalctl -u forgejo-runner -f"
  echo "(watch the first job run all the way through before trusting this is right — a"
  echo " missing PATH entry shows up as 'command not found' deep in a build step, not"
  echo " at startup.)"
fi

echo
echo "Done. Next: scripts/provision-windows-vm.sh (once, from this box) to stand"
echo "up the Windows guest, then run scripts/runner-setup-windows.ps1 inside it."
