# In-app updater — release & CI infrastructure

The app updates itself from **Codeberg** releases using the
[`self_update`](https://crates.io/crates/self_update) crate's **`manifest` backend**
(Codeberg/Forgejo has no dedicated backend, and a static manifest is forge-agnostic
anyway). Artifacts are signed with **zipsign** (ed25519); the app embeds the public
key and refuses an unsigned or mis-signed download.

App side lives in `crates/dtb-ke-ui/src/updater/`. CI side is
`.forgejo/workflows/{tip,release}.yml` + `dtb-ke-bundle`'s `codeberg`/`manifest`
subcommands — see **`RUNNERS.md`** for the runner architecture and secrets those
workflows need. This file covers the manifest schema, the two update channels,
and what the app does with them.

---

## 1. One-time: generate the signing key pair

```sh
cargo install zipsign
zipsign gen-key release.priv release.pub      # ed25519; .pub is 32 raw bytes
```

- Commit **`assets/release.pub`** (the 32-byte public key) to the repo. `build.rs`
  reads it into `updater::key::VERIFY_KEY`; with no file the whole updater is
  compiled out (`updater::available()` is `false`).
- Keep **`release.priv`** out of the repo. Add it as the `ZIPSIGN_KEY` CI secret
  (base64 the file: `base64 -w0 release.priv`) — see `RUNNERS.md`.
- To rotate: generate a new pair, ship an app build with the new `release.pub`,
  then sign subsequent releases with the new `release.priv`. Users on the old
  build stop updating until they install a build carrying the new key.

---

## 2. Two channels, two rolling tags

`Settings.update_channel` (`Stable` / `Tip`, `settings.rs`) picks which manifest
URL the app polls:

| channel | manifest URL | produced by | when |
|---|---|---|---|
| Stable (default) | `.../releases/download/latest/manifest.json` | `release.yml` | a `vX.Y.Z` tag pushed by `scripts/release.sh` |
| Tip / Nightly | `.../releases/download/tip/manifest.json` | `tip.yml` | every push to `main` |

Both `latest` and `tip` are **rolling tags** — each workflow force-moves its
tag to the new commit and replaces its release object every run
(`cargo dtb-ke-bundle codeberg prepare`; see `RUNNERS.md` for why the release
API can't retarget a tag itself and the workaround). A version tag like
`v0.2.0` also gets its own, permanent, non-rolling release — `latest` is a
second copy of the same assets under a stable URL the app can always poll
without knowing the newest version number in advance.

### Versioning

The workspace `Cargo.toml` `[workspace.package] version` sits at
**`X.Y.(Z+1)-dev.0`** between releases (`X.Y.Z` = the last release). `release.sh`
puts it there: it commits `X.Y.Z`, tags `vX.Y.Z`, then commits the
`X.Y.(Z+1)-dev.0` bump, and pushes the tag + both commits **atomically** — so
`latest` and `tip` diverge by exactly one commit, and `main`'s version is
semver-**strictly greater** than the release just cut.

| build | version it reports | how |
|---|---|---|
| **Release** (`release.yml`, checks out the `vX.Y.Z` tag) | `X.Y.Z` | tag commit's `CARGO_PKG_VERSION` |
| **Tip** (`tip.yml`, builds `main`) | `X.Y.(Z+1)-dev.N` | `DTB_KE_VERSION` env, per build job |
| **local** `cargo run` | `X.Y.(Z+1)-dev.0` | `CARGO_PKG_VERSION` |

`N` = `git rev-list --count --first-parent vX.Y.Z..HEAD` — commits on `main`
since the last release tag (or since the repo root before the first release).
Monotonic, resets itself at every release, derived from history so there is no
counter to store or bump. `self_update` compares digit-only prerelease
identifiers numerically, so `…-dev.7 < …-dev.12`. The `tip.yml` `prepare` job
checks out `fetch-depth: 0` for this; the build jobs stay shallow.

The Tip version is **baked into the binary** via `DTB_KE_VERSION` because
`CARGO_PKG_VERSION` alone is the unchanging `X.Y.(Z+1)-dev.0` for every Tip
build — the updater would then compare an installed `X.Y.(Z+1)-dev.0` against a
published `X.Y.(Z+1)-dev.12` and *would* see it as newer, but it would never
be able to tell `dev.12` from `dev.13`. `build_info::APP_VERSION` prefers
`option_env!("DTB_KE_VERSION")` over `CARGO_PKG_VERSION`; `build.rs` emits
`cargo:rerun-if-env-changed=DTB_KE_VERSION` so the persistent Tip target dir
doesn't cache a stale version in.

**Channel switching** has no memory — a check is purely `newest version in the
selected channel's manifest > installed APP_VERSION` (semver), plus
`Settings.skipped_update` is cleared on the flip. Consequences:

- **Stable → Tip** always finds an update: Tip is at least `X.Y.(Z+1)-dev.1`,
  which is `> X.Y.Z`.
- **Tip → Stable** either lands on the current release (`X.Y.Z` outranks any
  `X.Y.Z-dev.*` — and since a `dev.*` build was *building toward* that release,
  it's not a code downgrade), or finds nothing if the Tip build's base has
  already moved past the newest release (`X.Y.(Z+1)-dev.* > X.Y.Z`), in which
  case it rejoins Stable at the next release.

**`can_self_install(channel)`** (`updater/mod.rs`) is `true` for macOS and
Windows on both channels — only Linux users, on every channel, get a
"Herunterladen" link instead of an in-app install. Tip archives (and a
Release build cut without `MACOS_SIGN_IDENTITY` — see § 3) are only ad-hoc
signed, so a self-swapped `.app` does cost a first-run Gatekeeper prompt on
relaunch; accepted deliberately for a small-user-base app rather than
forcing a manual reinstall every Tip build. (Tip archives *are*
zipsign-signed — see § 3 — so `run_install` verifies the signature on both
channels; only the macOS code-signing / notarization differs.)

---

## 3. Per release: what CI produces

For a given tag (`vX.Y.Z` or the rolling `tip`), CI builds and uploads to the
Codeberg release:

| file | purpose |
|---|---|
| `DTB-Kampfrichtereinsatzpläne-<version>-universal-apple-darwin.tar.gz` | macOS (arm64 + x64 as one `lipo` binary) — **archive root is `DTB Kampfrichtereinsatzpläne.app`** (spaced — the shipped product name, `meta::DISPLAY_NAME`; the outer archive *file* name stays hyphenated) |
| `DTB-Kampfrichtereinsatzpläne-<version>-x86_64-pc-windows-gnullvm.zip` | Windows x64 — **archive root is `DTB Kampfrichtereinsatzpläne/`** (spaced) containing the `.exe` |
| `DTB-Kampfrichtereinsatzpläne-<version>-aarch64-pc-windows-gnullvm.zip` | Windows arm64 — same layout |
| `manifest.json` | the update manifest (below) |
| `DTB-Kampfrichtereinsatzpläne-<version>-{x86_64,aarch64}-unknown-linux-gnu.tar.gz` | Linux archives — **the updater never installs these**, only offers the download link |
| (release builds only) `.dmg` / `.msi` / `.deb` / `.rpm` / `.AppImage` | first-install artifacts — **the updater never touches these** either |

**Naming matters:** each updatable archive's file name must contain the Rust
target triple — that is how the asset is matched to the running platform. The
`.app` / folder name *inside* the archive must be exactly
`DTB Kampfrichtereinsatzpläne` (spaced — `updater::BIN_NAME`, matching
`dtb-ke-bundle::meta::DISPLAY_NAME`; **not** the kebab-case `RAW_BIN_NAME`
cargo itself builds — see CLAUDE.md's Bundling section).

**macOS asset matching** (`updater::asset_priority`, wired into both
`fetch_newest` and `run_install`'s `self_update` `asset_matcher`): the CI
macOS artifact is a single `universal-apple-darwin` fat archive, so
`self_update`'s built-in matcher — which looks for the native triple
(`aarch64-apple-darwin` / `x86_64-apple-darwin`) — finds nothing and the
update fails. `asset_priority` ranks candidates: an exact native-triple match
first (priority 0), then the arch+os tokens (1), then — macOS only — a
`universal-apple-darwin` name (2). So a per-arch slice always wins if one is
ever published, but a universal-only release still updates. The chosen asset
keeps its manifest `digest`, so `verify_release_digest` is unaffected. No
`self_update` fork — `manifest::UpdateBuilder::asset_matcher` is a public
hook.

### Signing (zipsign — both channels; macOS code-signing differs, see `RUNNERS.md`)

Every updatable archive on **both** channels is zipsign-signed in CI
(`.forgejo/workflows/{tip,release}.yml`, using the `ZIPSIGN_KEY` secret):

```sh
zipsign sign tar "$archive" release.priv        # .tar.gz
zipsign sign zip "$archive" release.priv        # .zip
```

`zipsign sign` appends the signature to the archive in place (still a valid
tar.gz / zip). `self_update`'s `signatures` feature verifies it against the
embedded key before extracting — and, once `verifying_keys` is set (which
`run_install` does unconditionally), it *requires* one: a missing signature
is `SignatureError: could not find read signatures`, not a skip. This is why
Tip archives are signed too — the alternative (dropping `verifying_keys` for
Tip) would leave a Tip install gated by the manifest digest alone, and the
private key is a CI secret regardless. macOS code-signing / notarization is a
separate axis (§ 2 / `RUNNERS.md`): Tip and a secret-less Release stay ad-hoc.

### Manifest

```sh
cargo dtb-ke-bundle manifest \
    --version X.Y.Z \
    --date "$(date -u +%Y-%m-%d)" \
    --notes-url "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/tag/vX.Y.Z" \
    --out manifest.json \
    dist/DTB-Kampfrichtereinsatzpläne-X.Y.Z-*.tar.gz \
    dist/DTB-Kampfrichtereinsatzpläne-X.Y.Z-*.zip
```

In CI this whole step is folded into `cargo dtb-ke-bundle codeberg
manifest-publish`, which merges the per-arch `<name>.fragment.json` sidecars
each upload step dropped on the release (rather than needing every archive
downloaded onto one runner to build the manifest directly) — see `RUNNERS.md`
for the fragment/publish job shape.

Emits schema-1 JSON:

```json
{
  "schema": 1,
  "releases": [
    {
      "version": "X.Y.Z",
      "date": "2026-09-10",
      "notes_url": "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene/releases/tag/vX.Y.Z",
      "assets": [
        { "name": "…-universal-apple-darwin.tar.gz", "url": "…", "size": 35651584,
          "digest": "sha256:…" }
      ]
    }
  ]
}
```

`size` is our extension (the app shows it in the toast; `self_update` ignores it).
`digest` is verified by `self_update`'s `checksums` feature *in addition to* the
signature (when present — Tip archives carry a digest but no signature).

Asset `url`s are relative → resolved against the manifest's own URL, so the
archives must live in the same release as the manifest — which they do,
since every upload in a given workflow run targets the same `release_id`(s).

---

## 4. What the app does

- **Check:** `updater::check` resolves the channel's manifest URL
  (`Settings.update_channel` → `manifest_url()`), then `fetch_newest` GETs it
  (`ureq`), parses it, and picks the highest release whose version is
  `> build_info::APP_VERSION` (see § 3 "Versioning" — `CARGO_PKG_VERSION`
  unless CI baked in a `DTB_KE_VERSION` override) and `>`
  any version the user chose to "Skip", via
  `self_update::version::bump_is_greater`. Runs ~4 s after launch, then every
  6 h, and on demand from *DTB Kampfrichtereinsatzpläne → Nach Updates
  suchen*. Gated by the *Einstellungen → Aktualisierung* toggle (default on);
  skips re-checking automatically while a result (`Available`/`Failed`/
  `UpToDate`) is already on screen.
  - A **404** on the manifest URL is treated as "no update available"
    (`Ok(None)`), not an error: a rolling channel tag legitimately has
    nothing published yet — `latest` until the first `release.yml` run, `tip`
    until the first `tip.yml` run. So a Stable-channel user before the first
    tagged release sees "Neueste Version bereits installiert" on a manual
    check, not "Aktualisierung fehlgeschlagen". (Codeberg's `latest` and
    `tip` are *separate* releases — being on Stable while only `tip` exists
    is the usual cause.)
- **Toast:** a corner pill / card (`updater::toast`) — Version, Größe,
  Veröffentlicht, and *Überspringen* / *Später* / *Installieren & neu starten*
  (or *Herunterladen* when `can_self_install` is false for this platform +
  channel). Clicking outside the expanded card collapses it back to the pill.
- **Install** (macOS + Windows, both channels): `self_update::
  backends::manifest::Update` re-fetches, downloads the archive
  (`asset_priority` chooses it — native triple first, then a
  `universal-apple-darwin` fat archive on macOS), verifies its zipsign
  signature and manifest digest (both, on both channels), and swaps the
  `.app` bundle / Windows folder as one unit. Download progress is reported
  through `self_update`'s `progress_callback` into `State::Installing { progress }`
  and drawn as a bar in the toast card. The app then flushes every open
  competition and re-execs the new binary.
- **macOS:** when the downloaded `.app` is **Developer-ID signed and
  notarized** (a Release build with `MACOS_SIGN_IDENTITY` set), Gatekeeper
  trusts it on relaunch without a fresh quarantine prompt. An ad-hoc-signed
  `.app` (Tip, always; Release when that secret is unset) still installs —
  `can_self_install` is `true` on macOS for both channels, see § 2 — it just
  costs a one-time "unidentified developer" Gatekeeper prompt on first
  launch after the swap, accepted deliberately for this app's small user
  base.
- **Settings:** the channel switch (Allgemein tab) clears
  `Settings.skipped_update` on change, since a skipped Stable version has no
  bearing on the Tip channel's version numbering or vice versa.

---

## 5. Testing without CI

```sh
DTB_KE_UPDATE_MANIFEST=http://localhost:PORT/manifest.json cargo run -p dtb-ke-ui
```

with a fake 32-byte `assets/release.pub` and `python3 -m http.server` (or the
`updater-testbed.sh` script this project used to build the feature) serving a
hand-written `manifest.json`. Watch for a stale `Settings.skipped_update`
matching your testbed's advertised version — the Settings window has a
"Zurücksetzen" control for exactly that.
