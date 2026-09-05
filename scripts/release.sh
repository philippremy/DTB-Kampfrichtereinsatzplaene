#!/usr/bin/env bash
# Cuts a release: bumps the workspace version, collects release notes +
# contributors, tags, and pushes. Pushing the tag is the *entire* trigger —
# .forgejo/workflows/release.yml does the actual Codeberg API orchestration
# (draft release, per-arch builds, signing/notarization, asset upload,
# manifest publish) once it sees the `vX.Y.Z` tag. This script never talks to
# the Codeberg API itself; it only has to get the tag + its message right.
#
#   scripts/release.sh [new-version]      e.g. scripts/release.sh 0.2.0
#
# See RUNNERS.md for the secrets release.yml needs and UPDATER.md for how the
# resulting manifest/versioning is consumed.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

have() { command -v "$1" >/dev/null 2>&1; }
die() { echo "release.sh: $*" >&2; exit 1; }

# Rewrite the first `version = "…"` line under [workspace.package] to $1.
# awk, not `sed -i`, because BSD/macOS sed has neither the GNU `0,/re/`
# address nor `{ … }` command grouping (`sed: bad flag … '}'`).
set_workspace_version() {
  awk -v v="$1" '
    /^\[workspace\.package\]/ { in_pkg = 1 }
    in_pkg && !done && /^version = "/ { sub(/"[^"]*"/, "\"" v "\""); done = 1 }
    { print }
  ' "$CARGO_TOML" > "${CARGO_TOML}.tmp" && mv "${CARGO_TOML}.tmp" "$CARGO_TOML"
}

have git || die "git is required"
[[ -d .git ]] || die "not a git repository — run this from the project root after the repo migration is done"

# ── 0. sanity checks ────────────────────────────────────────────────────
branch="$(git rev-parse --abbrev-ref HEAD)"
[[ "$branch" == "main" ]] || die "must be on 'main' (currently on '$branch')"
[[ -z "$(git status --porcelain)" ]] || die "working tree not clean — commit or stash first"

echo "fetching origin to check we're up to date …"
# `--force`: the repo has rolling tags (`tip`, `latest`) that CI keeps
# moving, so a plain `--tags` fetch fails with "would clobber existing tag".
# Version tags (`vX.Y.Z`) never move, so forcing local tags to match origin
# is safe here.
git fetch origin main --tags --force
local_head="$(git rev-parse HEAD)"
remote_head="$(git rev-parse origin/main)"
[[ "$local_head" == "$remote_head" ]] || die "local main is not in sync with origin/main — pull first"

# ── 1. figure out the current + new version ─────────────────────────────
CARGO_TOML="Cargo.toml"
grep -q '^\[workspace.package\]' "$CARGO_TOML" || die "expected [workspace.package] in $CARGO_TOML"
current_version="$(awk '/^\[workspace.package\]/{f=1} f && /^version = /{gsub(/"/,"",$3); print $3; exit}' "$CARGO_TOML")"
[[ -n "$current_version" ]] || die "could not read the current version from $CARGO_TOML"
echo "current version: $current_version"

new_version="${1:-}"
if [[ -z "$new_version" ]]; then
  read -r -p "New version (semver, no leading 'v') [current: $current_version]: " new_version
fi
[[ "$new_version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "'$new_version' doesn't look like a plain x.y.z semver"
[[ "$new_version" != "$current_version" ]] || die "new version equals the current one"
tag="v${new_version}"
git rev-parse "$tag" >/dev/null 2>&1 && die "tag $tag already exists"

# ── 2. figure out the commit range + collect notes material ─────────────
last_tag="$(git tag -l 'v*' --sort=-v:refname | head -1 || true)"
if [[ -n "$last_tag" ]]; then
  range="${last_tag}..HEAD"
  echo "commit range since last release ($last_tag): $range"
else
  range="HEAD"
  echo "no previous version tag found — this looks like the first release"
fi

commits="$(git log $range --no-merges --format='- %s (%h)' 2>/dev/null || true)"
contributors="$(git shortlog -sne $range 2>/dev/null | sed 's/^[[:space:]]*[0-9]*[[:space:]]*/- /' || true)"

# ── 3. open $EDITOR on a pre-filled notes template ───────────────────────
NOTES_FILE="$(mktemp /tmp/dtb-ke-release-notes.XXXXXX.md)"
trap 'rm -f "$NOTES_FILE"' EXIT

cat > "$NOTES_FILE" <<EOF
# Release notes for $tag. Lines starting with "# " (hash-space) are these
# instructions and are stripped automatically — no need to delete them.
# Markdown headings ("## …") are kept. Write the notes below as plain prose
# and "- " bullets; what remains becomes the annotated tag message and, from
# there, the Codeberg release body. The generated lists are a starting point,
# not the final wording. Save + exit to continue; leave the file empty (or
# with only these "# " lines) to abort — nothing is tagged or pushed.

## Changes

$commits

## Contributors

$contributors
EOF

editor="${EDITOR:-${VISUAL:-nano}}"
have "$editor" || die "\$EDITOR ('$editor') not found — set EDITOR to something on PATH"
"$editor" "$NOTES_FILE"

# Strip the "# " instruction lines (keep "## " headings), trim leading blanks,
# then check there's real content left.
notes="$(grep -v '^# ' "$NOTES_FILE" | sed '/./,$!d')"
if [[ -z "$(echo "$notes" | tr -d '[:space:]')" ]]; then
  die "release notes are empty — aborted, nothing was tagged or pushed"
fi
echo "$notes" > "$NOTES_FILE"

echo
echo "── release notes ──"
cat "$NOTES_FILE"
echo "───────────────────"
read -r -p "Tag $tag with these notes and push? [y/N] " confirm
[[ "$confirm" =~ ^[Yy]$ ]] || die "aborted by user"

# ── 4. release commit + tag ─────────────────────────────────────────────
set_workspace_version "$new_version"
grep -qF "version = \"$new_version\"" "$CARGO_TOML" || die "version bump failed — check $CARGO_TOML manually"
# The bump touches Cargo.lock too (workspace members inherit the version).
have cargo && cargo update --workspace --offline >/dev/null 2>&1 || true
git add "$CARGO_TOML" Cargo.lock 2>/dev/null || git add "$CARGO_TOML"
git commit -m "chore: release $tag"
git tag -a -F "$NOTES_FILE" "$tag"

# ── 5. bump to the next patch's -dev.0 ──────────────────────────────────
#      `X.Y.Z` → `X.Y.(Z+1)-dev.0`: semver-strictly-greater than the release
#      just cut (so switching to the Tip channel always finds an update),
#      and `main` — hence every Tip build — sits exactly one commit ahead of
#      `latest`. tip.yml turns this into `X.Y.(Z+1)-dev.<commits-since-tag>`.
IFS=. read -r _maj _min _pat <<<"$new_version"
next_dev="${_maj}.${_min}.$((_pat + 1))-dev.0"
set_workspace_version "$next_dev"
have cargo && cargo update --workspace --offline >/dev/null 2>&1 || true
git add "$CARGO_TOML" Cargo.lock 2>/dev/null || git add "$CARGO_TOML"
git commit -m "chore: bump to $next_dev"

# ── 6. push — the release commit + tag + the dev bump, all at once, so the
#      tag push triggers release.yml and the same main push triggers tip.yml
#      with the -dev.0 base already in place.
echo "pushing main + $tag …"
git push --atomic origin main "$tag"

cat <<EOF

Done. $tag is pushed — CI takes it from here:
  https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/actions

  release.yml (the tag)  → builds every target, signs + notarizes macOS,
                           publishes under $tag and the rolling 'latest'.
  tip.yml  (the main push) → publishes ${next_dev%-dev.0}-dev.1 to 'tip'.

main is at $next_dev, one commit ahead of $tag and semver-greater than it.
EOF
