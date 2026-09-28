#!/usr/bin/env python3
"""Cuts a release: bumps the workspace version, collects release notes + contributors, tags, and
pushes. Pushing the tag is the *entire* trigger -- .forgejo/workflows/release.yml does the actual
Codeberg API orchestration (draft release, per-arch builds, signing/notarization, asset upload,
manifest publish) once it sees the `vX.Y.Z` tag. This script never talks to the Codeberg API itself;
it only has to get the tag + its message right.

Ports `scripts/release.sh`.

    release.py [--force] [new-version]     e.g. release.py 0.2.0

--force re-cuts an *existing* version: it moves the `vX.Y.Z` tag to a fresh release commit and
force-pushes it. Use it to re-trigger release.yml after a CI-config fix; the old `chore:` commits
stay in history until you rebase them out.

See RUNNERS.md for the secrets release.yml needs and UPDATER.md for how the resulting
manifest/versioning is consumed.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
import tempfile
import time
from pathlib import Path

WORKSPACE_ROOT = Path(__file__).resolve().parent.parent
CARGO_TOML = WORKSPACE_ROOT / "Cargo.toml"
# AppStream metainfo, read by appimagetool/appstreamcli at bundle time -- Cargo.toml isn't shipped
# in the AppImage, so its own <release version="…" date="…"/> needs updating separately.
METAINFO_FILES = [
    WORKSPACE_ROOT / "crates/dtb-ke-ui/appimage-metainfo.xml",
    WORKSPACE_ROOT / "crates/dtb-ke-debugger/appimage-metainfo.xml",
]


class Die(Exception):
    pass


def die(msg: str) -> None:
    raise Die(msg)


def run(args: list[str], **kwargs) -> subprocess.CompletedProcess:
    return subprocess.run(args, **kwargs)


def push_retry(args: list[str]) -> None:
    """`git push` + args, retried once. Codeberg intermittently rejects the first push of a
    session (a known upstream bug); a second attempt goes through."""
    if run(["git", "push", *args]).returncode == 0:
        return
    print("release.py: push failed — retrying once in 5s (known Codeberg flakiness) …", file=sys.stderr)
    time.sleep(5)
    run(["git", "push", *args], check=True)


def set_workspace_version(new_version: str) -> None:
    """Rewrite the first `version = "…"` line under [workspace.package] to new_version."""
    lines = CARGO_TOML.read_text().splitlines(keepends=True)
    in_pkg = False
    done = False
    for i, line in enumerate(lines):
        if line.startswith("[workspace.package]"):
            in_pkg = True
            continue
        if in_pkg and not done and line.startswith('version = "'):
            lines[i] = re.sub(r'"[^"]*"', f'"{new_version}"', line, count=1)
            done = True
    CARGO_TOML.write_text("".join(lines))


def current_version() -> str | None:
    in_pkg = False
    for line in CARGO_TOML.read_text().splitlines():
        if line.startswith("[workspace.package]"):
            in_pkg = True
            continue
        if in_pkg and line.startswith("version = "):
            m = re.search(r'"([^"]*)"', line)
            return m.group(1) if m else None
    return None


def set_metainfo_release(version: str) -> None:
    """Rewrite each AppStream metainfo's `<release version="…" date="…"/>` to the release being
    cut. Only called for the real `X.Y.Z` bump -- a "-dev.0" version has no place in AppStream's
    release history, so the post-release dev bump leaves these files alone."""
    today = time.strftime("%Y-%m-%d")
    pattern = re.compile(r'<release version="[^"]*" date="[^"]*"\s*/>')
    replacement = f'<release version="{version}" date="{today}"/>'
    for path in METAINFO_FILES:
        text = path.read_text()
        new_text, count = pattern.subn(replacement, text, count=1)
        if count != 1:
            die(f"could not find a <release …/> line to update in {path}")
        path.write_text(new_text)


def git_add_best_effort(*paths: str) -> None:
    """`git add <paths…> 2>/dev/null || git add <first path>` -- Cargo.lock may not exist / may
    not have changed; fall back to just the Cargo.toml add if the combined one fails."""
    if run(["git", "add", *paths], stderr=subprocess.DEVNULL).returncode != 0:
        run(["git", "add", paths[0]], check=True)


def cargo_update_best_effort() -> None:
    if shutil.which("cargo"):
        run(["cargo", "update", "--workspace", "--offline"], stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)


def bump_and_commit(new_version: str, commit_message: str, *, update_metainfo: bool = False) -> None:
    set_workspace_version(new_version)
    if f'version = "{new_version}"' not in CARGO_TOML.read_text():
        die(f"version bump failed — check {CARGO_TOML} manually")
    if update_metainfo:
        set_metainfo_release(new_version)
    # The bump touches Cargo.lock too (workspace members inherit the version).
    cargo_update_best_effort()
    git_add_best_effort(str(CARGO_TOML), "Cargo.lock")
    if update_metainfo:
        run(["git", "add", *(str(p) for p in METAINFO_FILES)], check=True)
    run(["git", "commit", "-m", commit_message], check=True)


def main(argv: list[str]) -> int:
    os.chdir(WORKSPACE_ROOT)

    if shutil.which("git") is None:
        print("release.py: git is required", file=sys.stderr)
        return 1
    if not (WORKSPACE_ROOT / ".git").is_dir():
        print("release.py: not a git repository — run this from the project root", file=sys.stderr)
        return 1

    # ── args: [--force] [new-version] in any order ──────────────────────
    force = False
    new_version = ""
    for arg in argv[1:]:
        if arg in ("-f", "--force"):
            force = True
        elif arg.startswith("-"):
            print(f"release.py: unknown option: {arg}", file=sys.stderr)
            return 1
        elif not new_version:
            new_version = arg
        else:
            print(f"release.py: unexpected argument: {arg}", file=sys.stderr)
            return 1

    notes_file: Path | None = None
    try:
        # ── 0. sanity checks ─────────────────────────────────────────────
        branch = run(["git", "rev-parse", "--abbrev-ref", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
        if branch != "main":
            die(f"must be on 'main' (currently on '{branch}')")

        print("fetching origin to check we're up to date …")
        # `--force`: the repo has rolling tags (`tip`, `latest`) that CI keeps moving, so a plain
        # `--tags` fetch fails with "would clobber existing tag". Version tags (`vX.Y.Z`) never
        # move, so forcing local tags to match origin is safe here.
        run(["git", "fetch", "origin", "main", "--tags", "--force"], check=True)

        # ── 1. figure out the current + new version ──────────────────────
        if "[workspace.package]" not in CARGO_TOML.read_text():
            die(f"expected [workspace.package] in {CARGO_TOML}")
        cur = current_version()
        if not cur:
            die(f"could not read the current version from {CARGO_TOML}")
        print(f"current version: {cur}")

        if not new_version:
            new_version = input(f"New version (semver, no leading 'v') [current: {cur}]: ").strip()
        if not re.match(r"^\d+\.\d+\.\d+$", new_version):
            die(f"'{new_version}' doesn't look like a plain x.y.z semver")
        if new_version == cur:
            die("new version equals the current one")
        tag = f"v{new_version}"

        retag = run(["git", "rev-parse", tag], capture_output=True).returncode == 0
        if retag:
            if not force:
                die(f"tag {tag} already exists (pass --force to move it and re-cut)")
            print(f"--force: {tag} will be re-pointed at a fresh release commit and force-pushed")

        # ── 2. gather the notes prefill ───────────────────────────────────
        if retag:
            print(f"re-cut of {tag} — seeding the editor with its current notes")
            contents = run(
                ["git", "tag", "-l", "--format=%(contents)", tag], capture_output=True, text=True
            ).stdout
            kept_lines = []
            for line in contents.splitlines():
                if line == "-----BEGIN PGP SIGNATURE-----":
                    break
                kept_lines.append(line)
            prefill = "\n".join(kept_lines)
        else:
            tags = run(
                ["git", "tag", "-l", "v*", "--sort=-v:refname"], capture_output=True, text=True
            ).stdout.splitlines()
            last_tag = tags[0] if tags else None
            if last_tag:
                rng = f"{last_tag}..HEAD"
                print(f"commit range since last release ({last_tag}): {rng}")
            else:
                rng = "HEAD"
                print("no previous version tag found — this looks like the first release")
            commits = run(
                ["git", "log", rng, "--no-merges", "--format=- %s (%h)"], capture_output=True, text=True
            ).stdout.strip()
            contributors_raw = run(["git", "shortlog", "-sne", rng], capture_output=True, text=True).stdout
            contributors = "\n".join(
                re.sub(r"^\s*\d+\s*", "- ", line) for line in contributors_raw.splitlines()
            )
            prefill = f"## Changes\n\n{commits}\n\n## Contributors\n\n{contributors}"

        # ── 3. open $EDITOR on a pre-filled notes template ────────────────
        fd, notes_path = tempfile.mkstemp(prefix="dtb-ke-release-notes.", suffix=".md")
        os.close(fd)
        notes_file = Path(notes_path)
        notes_file.write_text(
            "// Release notes for "
            + tag
            + ". Lines starting with \"//\" are these instructions\n"
            "// and are stripped automatically — no need to delete them. \"//\" is not\n"
            "// Markdown syntax, so \"#\" first-level headings in the notes below are safe.\n"
            "// Write the notes as prose and \"- \" bullets; what remains becomes the\n"
            "// annotated tag message and, from there, the Codeberg release body. The\n"
            "// generated content is a starting point, not the final wording. Save + exit\n"
            "// to continue; leave the file empty (or with only these \"//\" lines) to\n"
            "// abort — nothing is tagged or pushed.\n"
            "\n" + prefill + "\n"
        )

        editor = os.environ.get("EDITOR") or os.environ.get("VISUAL") or "nano"
        if shutil.which(editor) is None:
            die(f"$EDITOR ('{editor}') not found — set EDITOR to something on PATH")
        run([editor, str(notes_file)], check=True)

        # Strip the "//" instruction lines (Markdown headings, "#" included, are kept), trim
        # leading blanks, then check there's real content left.
        kept = [l for l in notes_file.read_text().splitlines() if not re.match(r"^//( |$)", l)]
        while kept and kept[0].strip() == "":
            kept.pop(0)
        notes = "\n".join(kept)
        if not notes.strip():
            die("release notes are empty — aborted, nothing was tagged or pushed")
        notes_file.write_text(notes + "\n")

        print("\n── release notes ──")
        print(notes_file.read_text(), end="")
        print("───────────────────")
        prompt = (f"Re-cut {tag} (move + force-push the tag)" if retag else f"Tag {tag}") + " with these notes and push? [y/N] "
        confirm = input(prompt).strip()
        if confirm.lower() != "y":
            die("aborted by user")

        # ── 4. release commit + tag ────────────────────────────────────────
        bump_and_commit(new_version, f"chore: release {tag}", update_metainfo=True)
        # --cleanup=whitespace, NOT the `git tag` default of `strip` -- `strip` removes every line
        # starting with git's comment char ("#"), which would silently eat all the Markdown
        # "#"/"##" headings in the notes. (We already stripped our own "//" instruction lines above.)
        tag_flags = ["-a", "--cleanup=whitespace", "-F", str(notes_file)]
        if retag:
            tag_flags = ["-f", *tag_flags]
        run(["git", "tag", *tag_flags, tag], check=True)

        # ── 5. bump to the next patch's -dev.0 ─────────────────────────────
        #      `X.Y.Z` → `X.Y.(Z+1)-dev.0`: semver-strictly-greater than the release just cut (so
        #      switching to the Tip channel always finds an update), and `main` -- hence every Tip
        #      build -- sits exactly one commit ahead of `latest`. tip.yml turns this into
        #      `X.Y.(Z+1)-dev.<commits-since-tag>`.
        maj, minr, pat = new_version.split(".")
        next_dev = f"{maj}.{minr}.{int(pat) + 1}-dev.0"
        bump_and_commit(next_dev, f"chore: bump to {next_dev}")

        # ── 6. push -- the release commit + tag + the dev bump, all at once, so the tag push
        #      triggers release.yml and the same main push triggers tip.yml with the -dev.0 base
        #      already in place. On --force, `+` force-updates only the tag (main stays a
        #      fast-forward push).
        print(f"pushing main + {tag} …")
        if retag:
            push_retry(["--atomic", "--force", "origin", "main", f"+refs/tags/{tag}"])
        else:
            push_retry(["--atomic", "--force", "origin", "main", tag])

        tip_version = next_dev[: -len("-dev.0")] + "-dev.1"
        print(f"""
Done. {tag} is pushed — CI takes it from here:
  https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/actions

  release.yml (the tag)  → builds every target, signs + notarizes macOS,
                           publishes under {tag} and the rolling 'latest'.
  tip.yml  (the main push) → publishes {tip_version} to 'tip'.

main is at {next_dev}, one commit ahead of {tag} and semver-greater than it.
""")
        return 0
    except Die as exc:
        print(f"release.py: {exc}", file=sys.stderr)
        return 1
    except subprocess.CalledProcessError as exc:
        print(f"release.py: command failed: {' '.join(exc.cmd)}", file=sys.stderr)
        return 1
    finally:
        if notes_file is not None:
            notes_file.unlink(missing_ok=True)


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
