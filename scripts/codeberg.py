#!/usr/bin/env python3
"""Codeberg/Forgejo release management -- the CI release-publishing helpers.

Ports `crates/dtb-ke-bundle/src/codeberg.rs`. Stdlib-only except for the upload step, which shells
out to `curl` deliberately, not as a shortcut: a hand-rolled multipart body (first via Rust's `ureq`,
matching what `urllib.request` would also have to hand-roll) failed *twice* against the real
Codeberg API in the original Rust version -- a raw connection-abort, then (after adding
`Expect: 100-continue`) an outright HTTP 400 -- while `curl -F` has uploaded real archives
successfully on Linux, Windows, and macOS throughout this project's CI. Re-implementing that same
hand-rolled body in Python risks the identical failure for the identical reason; `curl`'s presence
here is an *empirically confirmed* fact about every CI runner, not an assumption.

The asset name can contain non-ASCII (this project's display name has "ä"), which broke `curl`
uploads too, but only on Windows: `curl.exe`'s ANSI-CRT argument parsing round-trips the outgoing
request text (the `?name=` query value, the `-F ...;filename=` field) through the process's ANSI
code page with no second UTF-8 conversion, so "ä" arrived on the wire as a single mangled byte --
invalid UTF-8, which Codeberg correctly rejected with 400. Percent-encoding the name before it ever
becomes a `curl` argument (`_url_encode`, RFC 3986 unreserved characters only) sidesteps this
entirely; Python's own `urllib.parse.quote(s, safe="")` already uses that exact character set, so it
needs no hand-rolling here the way the original Rust did.

Auth: `$CODEBERG_TOKEN` (a personal/repo access token with `write:repository` scope).

    codeberg.py prepare --tag TAG --target TARGET --title TITLE [--notes-file FILE] [--draft]
                [--prerelease]
        Replace any existing release for TAG (never the underlying git tag -- move a rolling tag
        with plain `git tag -f`/`git push -f` *before* calling this), print `release_id=<id>` to
        stdout and append it to $GITHUB_OUTPUT/$FORGEJO_OUTPUT if set.

    codeberg.py upload [--plain] --release-id ID [--release-id ID2 ...] <file>...
        Upload every file to every given release. Without --plain, also uploads a small
        `<name>.fragment.json` sidecar per file per release ({name, url, size, digest}) --
        manifest-publish later merges these without re-downloading the (large) archives. --plain is
        for assets that must never look like a downloadable app update (a debug-info archive).

    codeberg.py manifest-publish --release-id ID --version VERSION [--date DATE] [--notes-url URL]
                [--publish]
        Merge every `*.fragment.json` asset on the release into the real `manifest.json`, upload it,
        delete the fragments, and (--publish) flip the release off draft.
"""

from __future__ import annotations

import argparse
import json
import os
import subprocess
import sys
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import manifest as manifest_mod  # noqa: E402 -- sha256(), same as codeberg.rs reusing manifest::sha256

API_BASE = "https://codeberg.org/api/v1"
OWNER = "philippremy"
REPO = "DTB-Kampfrichtereinsatzplaene"


def _api_url(path: str) -> str:
    return f"{API_BASE}/repos/{OWNER}/{REPO}{path}"


def _request(method: str, url: str, token: str, json_body: dict | None = None) -> dict | list | None:
    data = json.dumps(json_body).encode("utf-8") if json_body is not None else None
    req = urllib.request.Request(
        url,
        data=data,
        method=method,
        headers={
            "Authorization": f"token {token}",
            **({"Content-Type": "application/json"} if data is not None else {}),
        },
    )
    with urllib.request.urlopen(req, timeout=60) as resp:
        body = resp.read()
        return json.loads(body) if body else None


def _find_by_tag(tag: str, token: str) -> dict | None:
    try:
        return _request("GET", _api_url(f"/releases/tags/{urllib.parse.quote(tag, safe='')}"), token)
    except urllib.error.HTTPError as e:
        if e.code == 404:
            return None
        raise RuntimeError(f"looking up release {tag}: {e.code} {e.reason}") from None


def _delete_release(release_id: int, token: str) -> None:
    try:
        _request("DELETE", _api_url(f"/releases/{release_id}"), token)
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"deleting release {release_id}: {e.code} {e.reason}") from None


def prepare(tag: str, target: str, title: str, notes: str, draft: bool, prerelease: bool, token: str) -> int:
    existing = _find_by_tag(tag, token)
    if existing is not None:
        print(f"codeberg.py: replacing existing release {tag} (id {existing['id']})", file=sys.stderr)
        _delete_release(existing["id"], token)

    body = {
        "tag_name": tag,
        "target_commitish": target,
        "name": title,
        "body": notes,
        "draft": draft,
        "prerelease": prerelease,
    }
    try:
        release = _request("POST", _api_url("/releases"), token, body)
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"creating release {tag}: {e.code} {e.reason} — {e.read().decode('utf-8', 'replace')}") from None

    release_id = release["id"]
    print(f"codeberg.py: created release {tag} (id {release_id})", file=sys.stderr)
    print(f"release_id={release_id}")
    out_path = os.environ.get("FORGEJO_OUTPUT") or os.environ.get("GITHUB_OUTPUT")
    if out_path:
        with open(out_path, "a") as f:
            f.write(f"release_id={release_id}\n")
    return release_id


def _url_encode(s: str) -> str:
    return urllib.parse.quote(s, safe="")


def _upload_raw(release_id: int, path: Path, token: str) -> str:
    """Uploads `path` as an asset of `release_id`, returns its `browser_download_url`. See the
    module doc comment for why this shells to `curl` rather than using `urllib.request` directly."""
    name = path.name
    encoded_name = _url_encode(name)
    url = f"{_api_url(f'/releases/{release_id}/assets')}?name={encoded_name}"
    result = subprocess.run(
        [
            "curl",
            "--fail",
            "--silent",
            "--show-error",
            "-X",
            "POST",
            "-H",
            f"Authorization: token {token}",
            "-F",
            f"attachment=@{path};filename={encoded_name}",
            url,
        ],
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"uploading {name}: curl exited {result.returncode} — {result.stderr.decode('utf-8', 'replace')}"
        )
    try:
        asset = json.loads(result.stdout)
    except json.JSONDecodeError as e:
        raise RuntimeError(f"parsing upload response for {name}: {e}") from None
    return asset["browser_download_url"]


def upload(release_ids: list[int], files: list[Path], token: str, plain: bool) -> None:
    for path in files:
        name = path.name
        size = path.stat().st_size
        digest = None if plain else manifest_mod.sha256(path)

        for release_id in release_ids:
            url = _upload_raw(release_id, path, token)
            suffix = ", no manifest fragment" if plain else ""
            print(
                f"codeberg.py: uploaded {name} to release {release_id} ({manifest_mod.human(size)}{suffix})",
                file=sys.stderr,
            )
            if plain:
                continue

            fragment = {"name": name, "url": url, "size": size, "digest": f"sha256:{digest}"}
            fragment_path = Path(f"{name}.fragment.json.tmp")
            fragment_path.write_text(json.dumps(fragment))
            try:
                _upload_raw_named(release_id, fragment_path, f"{name}.fragment.json", token)
            finally:
                fragment_path.unlink(missing_ok=True)


def _upload_raw_named(release_id: int, path: Path, upload_name: str, token: str) -> str:
    """Like `_upload_raw`, but uploads under `upload_name` instead of `path.name` -- the fragment is
    staged under a `.tmp` suffix locally (so it can never collide with a real archive of the same
    stem) but must be uploaded as `<name>.fragment.json`, matching what `manifest-publish` expects to
    find by suffix."""
    encoded_name = _url_encode(upload_name)
    url = f"{_api_url(f'/releases/{release_id}/assets')}?name={encoded_name}"
    result = subprocess.run(
        [
            "curl", "--fail", "--silent", "--show-error", "-X", "POST",
            "-H", f"Authorization: token {token}",
            "-F", f"attachment=@{path};filename={encoded_name}",
            url,
        ],
        capture_output=True,
    )
    if result.returncode != 0:
        raise RuntimeError(
            f"uploading {upload_name}: curl exited {result.returncode} — {result.stderr.decode('utf-8', 'replace')}"
        )
    return json.loads(result.stdout)["browser_download_url"]


def manifest_publish(release_id: int, version: str, date: str | None, notes_url: str | None, publish: bool, token: str) -> None:
    assets = _request("GET", _api_url(f"/releases/{release_id}/assets"), token) or []

    manifest_assets = []
    fragment_ids = []
    for asset in assets:
        if not asset["name"].endswith(".fragment.json"):
            continue
        req = urllib.request.Request(asset["browser_download_url"], headers={"Authorization": f"token {token}"})
        with urllib.request.urlopen(req, timeout=60) as resp:
            manifest_assets.append(json.loads(resp.read()))
        fragment_ids.append(asset["id"])

    if not manifest_assets:
        raise RuntimeError("no *.fragment.json assets found — did every build job's upload step run?")

    release = {"version": version}
    if date:
        release["date"] = date
    if notes_url:
        release["notes_url"] = notes_url
    release["assets"] = manifest_assets
    manifest = {"schema": 1, "releases": [release]}

    manifest_path = Path("manifest.json.tmp")
    manifest_path.write_text(json.dumps(manifest, indent=2))
    try:
        existing = next((a for a in assets if a["name"] == "manifest.json"), None)
        if existing is not None:
            try:
                _request("DELETE", _api_url(f"/releases/{release_id}/assets/{existing['id']}"), token)
            except urllib.error.HTTPError:
                pass  # a rerun of a failed publish step must not fail on "asset already deleted"
        _upload_raw_named(release_id, manifest_path, "manifest.json", token)
    finally:
        manifest_path.unlink(missing_ok=True)

    for fragment_id in fragment_ids:
        try:
            _request("DELETE", _api_url(f"/releases/{release_id}/assets/{fragment_id}"), token)
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"deleting fragment asset {fragment_id}: {e.code} {e.reason}") from None

    if publish:
        try:
            _request("PATCH", _api_url(f"/releases/{release_id}"), token, {"draft": False})
        except urllib.error.HTTPError as e:
            raise RuntimeError(f"publishing release {release_id}: {e.code} {e.reason}") from None
        print(f"codeberg.py: release {release_id} published", file=sys.stderr)

    print(f"codeberg.py: manifest.json written ({len(manifest_assets)} asset(s))", file=sys.stderr)


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)

    p_prepare = sub.add_parser("prepare")
    p_prepare.add_argument("--tag", required=True)
    p_prepare.add_argument("--target", required=True)
    p_prepare.add_argument("--title", required=True)
    p_prepare.add_argument("--notes-file")
    p_prepare.add_argument("--draft", action="store_true")
    p_prepare.add_argument("--prerelease", action="store_true")

    p_upload = sub.add_parser("upload")
    p_upload.add_argument("--plain", action="store_true")
    p_upload.add_argument("--release-id", action="append", type=int, required=True, dest="release_ids")
    p_upload.add_argument("files", nargs="+", type=Path)

    p_publish = sub.add_parser("manifest-publish")
    p_publish.add_argument("--release-id", required=True, type=int)
    p_publish.add_argument("--version", required=True)
    p_publish.add_argument("--date")
    p_publish.add_argument("--notes-url")
    p_publish.add_argument("--publish", action="store_true")

    args = parser.parse_args(argv[1:])
    token = os.environ.get("CODEBERG_TOKEN")
    if not token:
        print("codeberg.py: CODEBERG_TOKEN is not set", file=sys.stderr)
        return 2

    try:
        if args.command == "prepare":
            notes = Path(args.notes_file).read_text() if args.notes_file else ""
            prepare(args.tag, args.target, args.title, notes, args.draft, args.prerelease, token)
        elif args.command == "upload":
            files = [f for f in args.files if f.is_file()]
            for f in args.files:
                if f not in files:
                    print(f"codeberg.py: skipping upload arg {f} — not a file", file=sys.stderr)
            upload(args.release_ids, files, token, args.plain)
        elif args.command == "manifest-publish":
            manifest_publish(args.release_id, args.version, args.date, args.notes_url, args.publish, token)
    except RuntimeError as e:
        print(f"codeberg.py: {e}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
