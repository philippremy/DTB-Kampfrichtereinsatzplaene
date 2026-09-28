#!/usr/bin/env python3
"""Local testing rig for the in-app updater (see UPDATER.md for the real CI flow).

Ports `scripts/updater-testbed.sh`. Updated for the current bundler (`cargo cargo-bundle`, the
vendored fork -- see vendor/cargo-bundle -- and `scripts/manifest.py`), not the retired
`dtb-ke-bundle` the original shelled out to.

Serves a manifest.json (+ optionally a real signed archive) from a throwaway HTTP server on
localhost, so the running app can be pointed at it with DTB_KE_UPDATE_MANIFEST -- no Codeberg
release, no real keypair required for the default mode.

    updater_testbed.py [check|full] [options]

    check (default)  Fastest. A throwaway 32-byte key (not a real signing key) + a hand-written
                      manifest advertising a newer version. Exercises the check -> toast ->
                      Skip/Later flow. Clicking "Installieren" will correctly FAIL (no real
                      signature) -- that's expected; check mode never builds an archive.

    full              Needs the `zipsign` CLI (`cargo install zipsign`). Generates a real ed25519
                      keypair, builds + bundles the app (debug by default -- pass --release for
                      the real thing), packages + signs a real archive, and writes a manifest that
                      points at it -- exercises the full download -> verify -> install -> relaunch
                      path. macOS only for now (the archive layout mirrors what CI will produce,
                      see UPDATER.md §2).

Options:
    --port <n>         HTTP port (default 8790)
    --version <x.y.z>  the "newer" version to advertise (default: current + 1 patch)
    --release          (full mode) bundle the release build instead of debug

Ctrl-C stops the server. Nothing here touches the committed assets/release.pub -- the throwaway key
lives under target/updater-testbed/ and is pointed to via DTB_KE_RELEASE_PUB, which build.rs reads
in preference to assets/release.pub.
"""

from __future__ import annotations

import datetime
import json
import os
import re
import shutil
import subprocess
import sys
import tarfile
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import manifest as manifest_mod  # noqa: E402

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
BIN_NAME = "dtb-ke-ui"  # the actual built/bundled executable name (cargo-bundle uses the real cargo
# binary name, not a transliterated display name the way the retired dtb-ke-bundle did)
DISPLAY_NAME = "DTB Kampfrichtereinsatzpläne"


def current_version() -> str:
    text = (WORKSPACE_ROOT / "Cargo.toml").read_text()
    m = re.search(r'^version = "(.*)"', text, re.MULTILINE)
    if not m:
        raise RuntimeError("no top-level version in Cargo.toml")
    return m.group(1)


def rustc_host() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    m = re.search(r"^host: (.+)$", out, re.MULTILINE)
    if not m:
        raise RuntimeError("could not determine rustc host triple")
    return m.group(1)


def main(argv: list[str]) -> int:
    os.chdir(WORKSPACE_ROOT)

    rest = argv[1:]
    if rest and not rest[0].startswith("--"):
        mode, rest = rest[0], rest[1:]
    else:
        mode = "check"

    port = 8790
    version_override = None
    release = False
    i = 0
    while i < len(rest):
        arg = rest[i]
        if arg == "--port":
            port = int(rest[i + 1])
            i += 2
        elif arg == "--version":
            version_override = rest[i + 1]
            i += 2
        elif arg == "--release":
            release = True
            i += 1
        else:
            print(f"updater-testbed: unknown option {arg}", file=sys.stderr)
            return 2

    current = current_version()
    if version_override:
        version = version_override
    else:
        maj, minr, patch = current.split(".")
        version = f"{maj}.{minr}.{int(patch) + 1}"

    bed = Path("target/updater-testbed")
    shutil.rmtree(bed, ignore_errors=True)
    bed.mkdir(parents=True)
    print(f"updater-testbed: mode={mode} current={current} advertising={version} port={port}")

    # The automatic check silently ignores a version the user already clicked "Überspringen" on --
    # only a manual "Nach Updates suchen" bypasses that. Since this script advertises the same
    # version (current + 1 patch) on every run unless told otherwise, a stale skip from an earlier
    # test session is a classic "auto-check finds nothing, manual check works" trap. Best-effort,
    # macOS only.
    settings_toml = Path.home() / "Library/Application Support/de.philippremy.DTB-Kampfrichtereinsatzpläne/Settings.toml"
    if settings_toml.is_file() and f'skipped_update = "{version}"' in settings_toml.read_text(errors="replace"):
        print(
            f"\n  ⚠ version {version} is already recorded as skipped in Settings.toml —\n"
            "    the AUTOMATIC check will silently ignore it (a manual 'Nach Updates\n"
            "    suchen' still finds it, which can look like the automatic path is\n"
            "    broken). Click 'Zurücksetzen' in Einstellungen → Aktualisierung, or\n"
            "    pass --version to advertise a different one.\n"
        )

    today = datetime.datetime.now(datetime.UTC).strftime("%Y-%m-%d")
    notes_url = f"https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/tag/v{version}"

    if mode == "check":
        # A 32-byte file is all build.rs checks -- it only needs to make updater::available() true;
        # check mode never calls run_install, so it is never used to verify anything.
        (bed / "release.pub").write_bytes(os.urandom(32))

        manifest = {
            "schema": 1,
            "releases": [
                {
                    "version": version,
                    "date": today,
                    "notes_url": notes_url,
                    "assets": [{"name": f"{BIN_NAME}-{version}-{rustc_host()}.tar.gz", "size": 35651584}],
                }
            ],
        }
        (bed / "manifest.json").write_text(json.dumps(manifest, indent=2))

        print(f"updater-testbed: building (DTB_KE_RELEASE_PUB={bed}/release.pub) …")
        env = os.environ.copy()
        env["DTB_KE_RELEASE_PUB"] = str((WORKSPACE_ROOT / bed / "release.pub").resolve())
        subprocess.run(["cargo", "build", "-p", "dtb-ke-ui"], env=env, check=True)

    elif mode == "full":
        if shutil.which("zipsign") is None:
            print("updater-testbed: 'zipsign' not found — cargo install zipsign", file=sys.stderr)
            return 1
        if sys.platform != "darwin":
            print("updater-testbed: full mode is macOS-only for now (see UPDATER.md §2)", file=sys.stderr)
            return 1

        subprocess.run(
            ["zipsign", "gen-key", str(bed / "release.priv"), str(bed / "release.pub")],
            stdout=subprocess.DEVNULL,
            check=True,
        )

        profile_dir = "release" if release else "debug"
        print(f"updater-testbed: building + bundling ({profile_dir}) — this can take a while …")
        env = os.environ.copy()
        env["DTB_KE_RELEASE_PUB"] = str((WORKSPACE_ROOT / bed / "release.pub").resolve())
        bundle_args = ["cargo", "cargo-bundle", "-p", "dtb-ke-ui", "-f", "osx"]
        if release:
            bundle_args.append("--release")
        subprocess.run(bundle_args, env=env, check=True)

        app_src = Path("target") / profile_dir / "bundle" / "osx" / f"{DISPLAY_NAME}.app"
        if not app_src.is_dir():
            print(f"updater-testbed: expected bundle at {app_src} — check 'cargo cargo-bundle' output above", file=sys.stderr)
            return 1

        target_triple = rustc_host()
        archive_name = f"{BIN_NAME}-{version}-{target_triple}.tar.gz"
        stage = bed / "stage"
        stage.mkdir(parents=True, exist_ok=True)
        # The archive's internal directory name must be $BIN_NAME.app -- that's what
        # updater::run_install's bundle_path_in_archive() looks for, independent of what the
        # bundler names the on-disk .app (the display name).
        staged_app = stage / f"{BIN_NAME}.app"
        shutil.copytree(app_src, staged_app)
        with tarfile.open(bed / archive_name, "w:gz") as tf:
            tf.add(staged_app, arcname=f"{BIN_NAME}.app")
        shutil.rmtree(stage, ignore_errors=True)

        print("updater-testbed: signing …")
        subprocess.run(["zipsign", "sign", "tar", str(bed / archive_name), str(bed / "release.priv")], check=True)

        print("updater-testbed: writing manifest …")
        manifest = manifest_mod.build_manifest(version, [bed / archive_name], date=today, notes_url=notes_url)
        (bed / "manifest.json").write_text(json.dumps(manifest, indent=2))

    else:
        print(f"updater-testbed: unknown mode '{mode}' (want 'check' or 'full')", file=sys.stderr)
        return 2

    # check mode always builds debug; full mode's binary matches --release.
    bin_path = Path("target") / ("release" if (mode == "full" and release) else "debug") / BIN_NAME

    print(f"\nupdater-testbed: serving {bed} on http://localhost:{port} (Ctrl-C to stop)\n")
    print("  In another terminal:")
    print(f"    DTB_KE_UPDATE_MANIFEST=http://localhost:{port}/manifest.json ./{bin_path}\n")

    os.chdir(bed)
    os.execvp("python3", ["python3", "-m", "http.server", str(port)])


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
