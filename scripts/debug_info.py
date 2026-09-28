#!/usr/bin/env python3
"""Packages a build's split-debuginfo sidecar into a single `.tar.gz`, kept entirely separate from
`cargo cargo-bundle`'s own packaging so debug symbols never leak into an installed `.deb`/`.rpm`/
`.AppImage`.

Ports `crates/dtb-ke-bundle/src/debug_info.rs` (+ the `debug_info_path`/`target_dir_for`/
`split_debug_file` helpers from `util.rs` this relies on). Where the sidecar lives:

  - macOS / iOS: `<name>.dSYM` next to the binary -- `[profile.release] split-debuginfo = "packed"`
    (workspace Cargo.toml) produces this on its own; nothing else needs to run first.
  - Linux: the standalone `<name>.debug` from `scripts/symbols.py split` (`objcopy
    --only-keep-debug`). Run that first for a release build -- this script only locates and archives
    what it left behind, it never runs the split itself.
  - Windows-gnullvm: the same `.debug` if the split produced one, else the *unstripped* executable
    `scripts/symbols.py split` copied aside (both from the same split step).

Never errors when there's nothing to package (a target that hasn't been built, or hasn't been split
yet) -- prints why and leaves the output unwritten. Best-effort extra material for offline
symbolication, never something that should fail a CI job on its own.

    debug_info.py [--debug] [--product app|debugger] [--universal | --target TRIPLE] <out.tar.gz>

    --universal   macOS only: merge the two --universal slices' .dSYM with lipo (both slices must
                  already be built -- this never builds anything itself).
"""

from __future__ import annotations

import argparse
import os
import shutil
import subprocess
import sys
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent

# raw_bin_name / raw_dsym_name from crates/dtb-ke-bundle/src/meta.rs -- the dSYM's inner DWARF
# binary is named after the crate (underscores), not the `[[bin]] name` (hyphens).
PRODUCTS = {
    "app": {"raw_bin_name": "dtb-ke-ui", "raw_dsym_name": "dtb_ke_ui"},
    "debugger": {"raw_bin_name": "dtb-ke-debugger", "raw_dsym_name": "dtb_ke_debugger"},
}

UNIVERSAL_TARGETS = ["x86_64-apple-darwin", "aarch64-apple-darwin"]


# --- path resolution (mirrors util.rs) ------------------------------------------------------------


def _cargo_target_root() -> Path:
    env = os.environ.get("CARGO_TARGET_DIR")
    if not env:
        return WORKSPACE_ROOT / "target"
    p = Path(env)
    return p if p.is_absolute() else WORKSPACE_ROOT / p


def target_dir_for(release: bool, target: str | None) -> Path:
    root = _cargo_target_root()
    profile = "release" if release else "debug"
    return root / target / profile if target else root / profile


def split_debug_file(release: bool, target: str | None, name: str) -> Path:
    return target_dir_for(release, target) / "stripped" / f"{name}.debug"


def exe_suffix(target: str | None) -> str:
    is_windows = "windows" in target if target else sys.platform == "win32"
    return ".exe" if is_windows else ""


def built_binary_path(release: bool, target: str | None, name: str) -> Path:
    return target_dir_for(release, target) / f"{name}{exe_suffix(target)}"


def debug_info_path(release: bool, target: str | None, name: str) -> Path | None:
    is_macos = ("apple-darwin" in target or "apple-ios" in target) if target else sys.platform == "darwin"
    is_linux = ("linux" in target) if target else sys.platform.startswith("linux")
    if is_macos:
        return target_dir_for(release, target) / f"{name}.dSYM"
    if is_linux:
        return split_debug_file(release, target, name)
    if target and "windows-gnullvm" in target:
        split = split_debug_file(release, target, name)
        return split if split.exists() else built_binary_path(release, target, name)
    return None


# --- packaging ---------------------------------------------------------------------------------


def _tar_entry(entry: Path, out: Path) -> None:
    """`tar -czf <out> -C <entry's parent> <entry's own name>` -- plain, portable flags (works with
    both GNU and BSD tar), handles a single file or a whole directory tree (macOS's .dSYM bundle)."""
    out.parent.mkdir(parents=True, exist_ok=True)
    result = subprocess.run(["tar", "-czf", str(out), "-C", str(entry.parent), entry.name])
    if result.returncode != 0:
        raise RuntimeError(f"tar exited {result.returncode}")


def _copy_tree(src: Path, dst: Path) -> None:
    if dst.exists():
        shutil.rmtree(dst)
    shutil.copytree(src, dst)


def merged_universal_dsym(release: bool, raw_bin_name: str, raw_dsym_name: str) -> Path | None:
    """Merges the two apple-darwin slices' `.dSYM`s into `target/universal/<profile>/<name>.dSYM`
    (inner DWARF binaries `lipo`-ed together). `None` (with a note) if a slice hasn't been built."""
    dsyms = []
    for triple in UNIVERSAL_TARGETS:
        path = debug_info_path(release, triple, raw_bin_name)
        if not path.exists():
            print(
                f"debug_info.py: expected a {triple} .dSYM at {path} but it doesn't exist — build "
                "with `cargo cargo-bundle --universal` first",
                file=sys.stderr,
            )
            return None
        dsyms.append(path)
    first, second = dsyms

    merged_dir = WORKSPACE_ROOT / "target" / "universal" / ("release" if release else "debug") / f"{raw_dsym_name}.dSYM"
    _copy_tree(first, merged_dir)

    dwarf_rel = Path("Contents/Resources/DWARF") / raw_dsym_name
    result = subprocess.run(
        ["lipo", "-create", "-output", str(merged_dir / dwarf_rel), str(first / dwarf_rel), str(second / dwarf_rel)]
    )
    if result.returncode != 0:
        raise RuntimeError(f"lipo exited {result.returncode}")
    return merged_dir


def report(path: Path) -> None:
    if path.is_dir():
        print(f"debug_info.py: ✔ {path}", file=sys.stderr)
    else:
        size = path.stat().st_size
        print(f"debug_info.py: ✔ {path} ({size} bytes)", file=sys.stderr)


def run(release: bool, universal: bool, target: str | None, out: Path, raw_bin_name: str, raw_dsym_name: str) -> None:
    if universal:
        merged = merged_universal_dsym(release, raw_bin_name, raw_dsym_name)
        if merged is None:
            return
        _tar_entry(merged, out)
        report(out)
        return

    path = debug_info_path(release, target, raw_bin_name)
    if path is None:
        print(
            "debug_info.py: this platform's split-debuginfo produces no sidecar file (see RUNNERS.md) "
            "— nothing to package",
            file=sys.stderr,
        )
        return
    if not path.exists():
        print(
            f"debug_info.py: expected a split-debuginfo sidecar at {path} but it doesn't exist — nothing to package",
            file=sys.stderr,
        )
        return

    _tar_entry(path, out)
    report(out)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--debug", action="store_true", help="package the debug build's sidecar (default: release)")
    parser.add_argument("--product", choices=["app", "debugger"], default="app")
    group = parser.add_mutually_exclusive_group()
    group.add_argument("--universal", action="store_true")
    group.add_argument("--target")
    parser.add_argument("out", type=Path)

    args = parser.parse_args(argv[1:])
    product = PRODUCTS[args.product]

    try:
        run(
            release=not args.debug,
            universal=args.universal,
            target=args.target,
            out=args.out,
            raw_bin_name=product["raw_bin_name"],
            raw_dsym_name=product["raw_dsym_name"],
        )
    except RuntimeError as e:
        print(f"debug_info.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
