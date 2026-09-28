#!/usr/bin/env python3
"""Split debug info off a release binary and upload it to the symbol server.

Ports `crates/dtb-ke-bundle/src/{strip,symbols}.rs`. Stdlib-only, no build step -- run directly as
`python3 scripts/symbols.py <split|upload> ...` from any CI runner (Linux, Windows, and -- for
`upload` only, macOS's dSYM never goes through `split` -- macOS too). Needs `scripts/debug_id.py`
next to it (the debug-id computation this reuses for both the split-time consistency check and the
upload's server-side key).

    split  <unstripped-binary> --target <triple> --out-dir <dir>
        Linux / Windows-gnullvm only. Copies the binary, splits a standalone debug file off the copy
        with objcopy/llvm-strip, strips the shipped copy, and verifies the shipped binary's debug id
        still matches the original build's (a mismatch means a future crash dump could never be
        matched to the uploaded debug file -- a hard error, not a warning). Prints
        `{"shipped": "...", "debug_file": "..."}` on success; the caller feeds `shipped` to
        `cargo cargo-bundle ... --binary-path` and `debug_file` to this script's own `upload`.

    upload <debug-file-or-dSYM-dir> [--server URL] [--token TOKEN] [--commit SHA] [--target TRIPLE]
           [--strict]
    upload --auto [--debug] [--product app|debugger] [--universal | --target TRIPLE] [--strict] ...
        PUTs the file (or, for a .dSYM directory, every file under Contents/Resources/DWARF/) to
        `{server}/v1/debug/{debug-id}`, one request per identity a file resolves to (a universal
        Mach-O slice has more than one). Server/token default to $DTB_KE_SYMBOLS_URL /
        $DTB_KE_SYMBOLS_UPLOAD_TOKEN. Best-effort by default (prints a warning and exits 0 on
        anything short of "every file uploaded"); --strict (release CI) makes that a hard failure.

        --auto resolves the sidecar itself instead of taking it as a positional argument -- the
        macOS/iOS dSYM `debug_info.debug_info_path` finds directly (`split-debuginfo = "packed"`
        already produced it, nothing else needed), or, on Linux/Windows-gnullvm, the `.debug` file
        (or, if the split couldn't produce a standalone one, the unstripped executable itself) an
        earlier `split` left under `<target-dir>/stripped/` -- same resolution rules as the old
        `cargo dtb-ke-bundle symbols upload [--universal | --target <t>]`.
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import debug_id  # noqa: E402
import debug_info  # noqa: E402


# --- split -----------------------------------------------------------------------------------


def _is_windows_gnullvm(target: str) -> bool:
    return "windows-gnullvm" in target


def _is_linux(target: str) -> bool:
    return "linux" in target


def _tool_candidates(base: str, target: str, host_arch: str) -> list[str]:
    """Mirrors `strip.rs::candidates` exactly, including the try order."""
    arch = target.split("-", 1)[0] if target else host_arch
    if _is_windows_gnullvm(target):
        return [f"llvm-{base}", f"{arch}-w64-mingw32-{base}", base]
    if arch == host_arch:
        return [base, f"llvm-{base}"]
    return [f"{arch}-linux-gnu-{base}", f"llvm-{base}"]


def _find_tool(base: str, target: str) -> str:
    candidates = _tool_candidates(base, target, os.uname().machine if hasattr(os, "uname") else "")
    for tool in candidates:
        try:
            ok = subprocess.run(
                [tool, "--version"], capture_output=True, check=False
            ).returncode == 0
        except FileNotFoundError:
            ok = False
        if ok:
            return tool
    raise RuntimeError(f"no {base} tool found (tried {', '.join(candidates)}) — install binutils or llvm")


def _run_tool(tool: str, args: list[str]) -> None:
    result = subprocess.run([tool, *args])
    if result.returncode != 0:
        raise RuntimeError(f"{tool} exited {result.returncode}")


def _ids(path: Path) -> set[str]:
    return {i.debug_id for i in debug_id.identify(path)}


def _extract_debug(objcopy: str, unstripped: Path, out: Path, before: set[str], dwarf_bytes: int) -> None:
    _run_tool(objcopy, ["--only-keep-debug", str(unstripped), str(out)])
    if before and _ids(out) != before:
        raise RuntimeError(f"the debug file has debug id(s) {_ids(out)}, the build has {before}")
    kept = debug_id.debug_section_bytes(out)
    if kept != dwarf_bytes:
        raise RuntimeError(f"the debug file keeps {kept} of the build's {dwarf_bytes} DWARF bytes")


def split(unstripped: Path, target: str, out_dir: Path) -> dict[str, str]:
    if not (_is_linux(target) or _is_windows_gnullvm(target)):
        raise RuntimeError(f"split only applies to Linux / Windows-gnullvm targets, not {target!r}")

    out_dir.mkdir(parents=True, exist_ok=True)
    shipped = out_dir / unstripped.name
    shutil.copy2(unstripped, shipped)

    before = _ids(unstripped)
    dwarf_bytes = debug_id.debug_section_bytes(unstripped)
    if dwarf_bytes == 0:
        print(f"symbols.py: WARNING {unstripped} carries no DWARF — there will be nothing to upload", file=sys.stderr)

    debug_stem = unstripped.stem  # matches util::split_debug_file's naming (binary name without .exe)
    debug_path = out_dir / f"{debug_stem}.debug"
    debug_path.unlink(missing_ok=True)

    if _is_windows_gnullvm(target):
        strip_tool = _find_tool("strip", target)
        print(f"symbols.py: stripping a copy with {strip_tool}", file=sys.stderr)
        _run_tool(strip_tool, ["--strip-all", str(shipped)])
        try:
            objcopy = _find_tool("objcopy", target)
            _extract_debug(objcopy, unstripped, debug_path, before, dwarf_bytes)
            debug_file = debug_path
        except RuntimeError as why:
            print(
                f"symbols.py: no standalone debug file for the PE ({why}) — keeping the unstripped executable",
                file=sys.stderr,
            )
            debug_path.unlink(missing_ok=True)
            debug_file = unstripped
    else:
        objcopy = _find_tool("objcopy", target)
        print(f"symbols.py: splitting the debug info with {objcopy}", file=sys.stderr)
        _extract_debug(objcopy, unstripped, debug_path, before, dwarf_bytes)
        _run_tool(objcopy, ["--strip-debug", "--strip-unneeded", str(shipped)])
        _run_tool(objcopy, [f"--add-gnu-debuglink={debug_path}", str(shipped)])
        debug_file = debug_path

    if not before:
        print(
            f"symbols.py: WARNING {unstripped} has no debug id (no ELF build-id / PE CodeView record) "
            "— its crash dumps cannot be matched to the uploaded debug file",
            file=sys.stderr,
        )
    elif _ids(shipped) != before:
        raise RuntimeError(
            f"the shipped binary {shipped} has debug id(s) {_ids(shipped)}, the build has {before} "
            "— dumps of it would not match the debug file"
        )
    print(
        f"symbols.py: shipped binary {shipped.stat().st_size} bytes, debug file {debug_file.stat().st_size} bytes",
        file=sys.stderr,
    )
    return {"shipped": str(shipped), "debug_file": str(debug_file)}


# --- upload ------------------------------------------------------------------------------------


def _candidate_files(sidecar: Path) -> list[Path]:
    if sidecar.is_file():
        return [sidecar]
    dwarf_dir = sidecar / "Contents" / "Resources" / "DWARF"
    return sorted(p for p in dwarf_dir.iterdir() if p.is_file())


def _put(server: str, token: str, debug_id_str: str, file: Path, commit: str | None, target: str | None) -> int:
    """`200` already stored (skipped), `2xx` newly stored. Raises on any other outcome."""
    url = f"{server}/v1/debug/{debug_id_str}"
    auth = {"Authorization": f"Bearer {token}"}

    head_req = urllib.request.Request(url, headers=auth, method="HEAD")
    try:
        with urllib.request.urlopen(head_req, timeout=30) as resp:
            if resp.status == 200:
                return 200
    except urllib.error.HTTPError as e:
        if e.code == 404:
            pass
        elif e.code in (401, 403):
            raise RuntimeError(f"server answered {e.code} — check the upload token") from None
        else:
            raise RuntimeError(f"server answered {e.code} to the pre-flight check") from None
    except urllib.error.URLError as e:
        raise RuntimeError(f"cannot reach the server: {e}") from None

    query = {"name": file.name}
    if commit:
        query["commit"] = commit
    if target:
        query["target"] = target
    put_url = f"{url}?{urllib.parse.urlencode(query)}"
    size = file.stat().st_size
    with open(file, "rb") as body:
        put_req = urllib.request.Request(
            put_url,
            data=body,
            headers={**auth, "Content-Length": str(size)},
            method="PUT",
        )
        try:
            with urllib.request.urlopen(put_req, timeout=1800) as resp:
                return resp.status
        except urllib.error.HTTPError as e:
            text = e.read().decode("utf-8", "replace").strip()
            raise RuntimeError(f"server answered {e.code}: {text}") from None
        except urllib.error.URLError as e:
            raise RuntimeError(str(e)) from None


def _git_commit() -> str | None:
    try:
        out = subprocess.run(
            ["git", "rev-parse", "HEAD"],
            cwd=Path(__file__).resolve().parent.parent,
            capture_output=True,
            check=False,
            text=True,
        )
    except FileNotFoundError:
        return None
    commit = out.stdout.strip()
    return commit if out.returncode == 0 and commit else None


def upload(sidecar: Path, server: str, token: str, commit: str | None, target: str | None, strict: bool) -> bool:
    files = _candidate_files(sidecar)
    commit = commit or _git_commit()
    uploaded = 0
    problems = 0

    for file in files:
        identities = debug_id.identify(file)
        if not identities:
            print(f"symbols.py: WARNING {file} has no debug id — skipped", file=sys.stderr)
            continue
        if debug_id.debug_section_bytes(file) == 0 and not str(file).endswith(".dSYM") and "DWARF" not in file.parts:
            print(f"symbols.py: WARNING {file} carries no debug info (stripped?) — skipped", file=sys.stderr)
            continue
        for identity in identities:
            try:
                status = _put(server, token, identity.debug_id, file, commit, target)
                print(f"symbols.py: ✔ symbol server: {identity.debug_id} ({status}) <- {file}", file=sys.stderr)
                uploaded += 1
            except RuntimeError as e:
                print(f"symbols.py: WARNING symbol upload of {identity.debug_id} failed: {e}", file=sys.stderr)
                problems += 1

    print(f"symbols.py: symbols upload done — {uploaded} uploaded, {problems} failed", file=sys.stderr)
    if strict and (problems > 0 or uploaded == 0):
        return False
    return True


# --- CLI -----------------------------------------------------------------------------------------


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    p_split = sub.add_parser("split")
    p_split.add_argument("unstripped", type=Path)
    p_split.add_argument("--target", required=True)
    p_split.add_argument("--out-dir", required=True, type=Path)

    p_upload = sub.add_parser("upload")
    p_upload.add_argument("sidecar", type=Path, nargs="?")
    p_upload.add_argument("--auto", action="store_true", help="resolve the sidecar automatically -- see above")
    p_upload.add_argument("--debug", action="store_true", help="--auto only: the debug build's sidecar (default: release)")
    p_upload.add_argument("--product", choices=["app", "debugger"], default="app", help="--auto only")
    p_upload.add_argument("--server", default=os.environ.get("DTB_KE_SYMBOLS_URL"))
    p_upload.add_argument("--token", default=os.environ.get("DTB_KE_SYMBOLS_UPLOAD_TOKEN"))
    p_upload.add_argument("--commit")
    group = p_upload.add_mutually_exclusive_group()
    group.add_argument("--universal", action="store_true", help="--auto only: macOS -- merge the two --universal dSYM slices")
    group.add_argument("--target")
    p_upload.add_argument("--strict", action="store_true")

    args = parser.parse_args(argv[1:])

    if args.command == "upload" and not args.auto and args.sidecar is None:
        print("symbols.py: upload needs a sidecar path, or --auto to resolve one", file=sys.stderr)
        return 2
    if args.command == "upload" and args.auto and args.sidecar is not None:
        print("symbols.py: upload takes a sidecar path OR --auto, not both", file=sys.stderr)
        return 2

    if args.command == "split":
        try:
            result = split(args.unstripped, args.target, args.out_dir)
        except RuntimeError as e:
            print(f"symbols.py: {e}", file=sys.stderr)
            return 1
        print(json.dumps(result))
        return 0

    if args.command == "upload":
        if not args.server or not args.token:
            message = "DTB_KE_SYMBOLS_URL / DTB_KE_SYMBOLS_UPLOAD_TOKEN not set (or --server/--token not passed)"
            if args.strict:
                print(f"symbols.py: symbols upload: {message}", file=sys.stderr)
                return 1
            print(f"symbols.py: symbols upload skipped — {message}", file=sys.stderr)
            return 0

        sidecar = args.sidecar
        if args.auto:
            product = debug_info.PRODUCTS[args.product]
            release = not args.debug
            try:
                if args.universal:
                    sidecar = debug_info.merged_universal_dsym(release, product["raw_bin_name"], product["raw_dsym_name"])
                else:
                    sidecar = debug_info.debug_info_path(release, args.target, product["raw_bin_name"])
                    if sidecar is not None and not sidecar.exists():
                        print(f"symbols.py: no debug file at {sidecar} — nothing to upload", file=sys.stderr)
                        sidecar = None
            except RuntimeError as e:
                print(f"symbols.py: {e}", file=sys.stderr)
                return 1
            if sidecar is None:
                if args.strict:
                    print("symbols.py: symbols upload: nothing to upload", file=sys.stderr)
                    return 1
                print("symbols.py: symbols upload skipped — this platform produces no debug file (or it hasn't been built/split yet)", file=sys.stderr)
                return 0

        server = args.server.rstrip("/")
        ok = upload(sidecar, server, args.token, args.commit, args.target, args.strict)
        return 0 if ok else 1

    return 2


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
