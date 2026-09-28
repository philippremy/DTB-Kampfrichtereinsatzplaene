#!/usr/bin/env python3
"""Build the `self_update` release manifest (schema 1).

Ports `crates/dtb-ke-bundle/src/manifest.rs`. Runs only on the Linux runner (per-arch archives from
every leg converge there before publishing). Stdlib-only.

    manifest.py --version 0.2.0 [--date 2026-09-10] [--notes-url URL] [--base-url URL]
                [--out manifest.json] <archive>...

Each archive's file name must contain the Rust target triple (how `self_update` matches the asset
to the running platform). `url` is `<base-url><file name>` when `--base-url` is given, else just the
file name (resolved by the app against the manifest's own URL).
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path


def sha256(path: Path) -> str:
    """Streamed, not `hashlib.file_digest`/reading the whole file at once -- these archives can be
    tens of MB and this also has to run identically wherever CI invokes it."""
    hasher = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(64 * 1024), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def human(num_bytes: int) -> str:
    units = ["B", "KB", "MB", "GB", "TB"]
    value = float(num_bytes)
    unit = 0
    while value >= 1024.0 and unit < len(units) - 1:
        value /= 1024.0
        unit += 1
    return f"{num_bytes} B" if unit == 0 else f"{value:.1f} {units[unit]}"


def build_manifest(
    version: str,
    archives: list[Path],
    date: str | None = None,
    notes_url: str | None = None,
    base_url: str | None = None,
) -> dict:
    assets = []
    for archive in archives:
        name = archive.name
        size = archive.stat().st_size
        digest = sha256(archive)
        url = f"{base_url.rstrip('/')}/{name}" if base_url else name
        assets.append({"name": name, "url": url, "size": size, "digest": f"sha256:{digest}"})

    release = {"version": version}
    if date:
        release["date"] = date
    if notes_url:
        release["notes_url"] = notes_url
    release["assets"] = assets
    return {"schema": 1, "releases": [release]}


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("archives", nargs="+", type=Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--date")
    parser.add_argument("--notes-url")
    parser.add_argument("--base-url")
    parser.add_argument("--out", type=Path, default=Path("manifest.json"))
    args = parser.parse_args(argv[1:])

    manifest = build_manifest(args.version, args.archives, args.date, args.notes_url, args.base_url)
    args.out.write_text(json.dumps(manifest, indent=2) + "\n")
    print(
        f"manifest.py: wrote {args.out} ({len(args.archives)} asset(s), version {args.version})",
        file=sys.stderr,
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
