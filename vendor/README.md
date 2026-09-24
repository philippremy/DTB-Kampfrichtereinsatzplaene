# Vendored dependency patches

Local, minimally-patched copies of crates.io packages, wired in via the
workspace root `Cargo.toml`'s `[patch.crates-io]` table. Used only when an
upstream bug actively breaks a real build and there's no environment-variable
or configuration escape hatch to work around it from our side — see
RUNNERS.md's "Vendored dependency patches" section for the fuller policy.

Each entry here is:

(Historical note: `turso_sdk_kit` used to be a plain copy of the published crate here; it is now a submodule + shim like taffy and zed, see the entries below. The description that follows is how that copy was originally made.)

1. The **exact published source** for the version currently in `Cargo.lock`
   — fetched from crates.io's own download endpoint
   (`https://crates.io/api/v1/crates/<name>/<version>/download`, needs a
   `User-Agent` header or crates.io returns 403), not a git checkout, so it's
   byte-identical to what was actually being built before patching (a
   crates.io package's published `Cargo.toml` is also already normalized —
   `.workspace = true` fields resolved to literal values — so it's a
   self-contained `path` dependency with no unmet workspace-inheritance).
2. Patched as **minimally as possible** — a targeted fix for the one bug
   that's actually blocking a build, not a general refresh or feature
   changes. Every patch is commented inline at the change site with why.
3. Removed the moment a fixed version ships upstream: bump the pinned
   version in the relevant `[workspace.dependencies]` entry, delete the
   `[patch.crates-io]` line and this crate's directory, `cargo update -p
   <crate>`, confirm the CI leg that originally caught the bug still passes.

## Current entries

- **`turso_sdk_kit` 0.7.2** (`vendor/turso` submodule + `vendor/turso_sdk_kit-shim`; the shim carries the published `Cargo.toml` and symlinks `src`/`build.rs` into `vendor/turso/sdk-kit`; the fix is one commit on the fork's `dtb-ke-patches` branch, on top of upstream commit `046e9cb`) — upstream's `build.rs` shells out to a bare
  `windres` (no target-triple prefix) to compile its Windows version
  resource, which resolves to whichever architecture's copy happens to be
  first on `PATH` — correct by coincidence on a native build, wrong when
  cross-compiling to a different architecture than the host. Confirmed
  against a real `aarch64-pc-windows-gnullvm` cross-build failure (`ld.lld:
  error: ...turso_sdk_kit_version.res: machine type x64 conflicts with
  arm64`) and the exact published source (no `CARGO_CFG_TARGET_ARCH` check,
  no prefix construction, anywhere in the file — verified before patching,
  not assumed). Patched to build the mingw-w64/llvm-mingw target-prefixed
  binary name (`<CARGO_CFG_TARGET_ARCH>-w64-mingw32-windres`) instead of the
  ambiguous bare one. One `Command::new(...)` call changed; everything else
  in the crate is untouched. Upstream: `github.com/tursodatabase/turso`,
  `sdk-kit/build.rs`.

## Vendored taffy (`vendor/taffy` + `vendor/taffy-shim`)

`vendor/taffy` is a shallow git submodule of `DioxusLabs/taffy` pinned at the
`v0.13.0` tag (gpui pins `taffy = "=0.13.0"`); `vendor/taffy-shim` is a wrapper
crate carrying the published `Cargo.toml` with `src` symlinked into the
submodule, wired in through `[patch.crates-io]` — the same scheme as
`gpui-pre-shims`. Local changes (all inside the submodule, marked "Local
change/addition (not upstream)"), needed by gpui's retained layout engine
(`vendor/zed/crates/gpui/src/taffy.rs`):

- `tree/cache.rs`: 4 ways per cache slot (upstream: 1). With one, two queries
  that share a slot evict each other every pass and no layout survives to the
  next frame, so every frame recomputed whole subtrees.
- `tree/taffy_tree.rs`: `TaffyTree::remove_detached` — order-free node removal
  that never clobbers a re-parented child's parent link.
- `compute/mod.rs`: cache hit/miss counters (`take_cache_stats`).

To update: check out the new tag in `vendor/taffy`, re-apply the three changes,
refresh `vendor/taffy-shim/Cargo.toml` from the published crate.

## Vendored gpui-base (`vendor/gpui-kit` + `vendor/gpui-base-shim`)

`vendor/gpui-kit` is a shallow git submodule of the `philippremy/gpui-kit` fork (upstream
`longbridge/gpui-kit`), checked out at `v0.6.6` — the commit `gpui-base` 0.6.6 was published
from — with the local changes on the `dtb-ke-patches` branch. `vendor/gpui-base-shim` wraps
`crates/base` with the published `Cargo.toml` and symlinks `src`/`tests`/`benches`, wired in via
`[patch.crates-io]` like the other vendored crates. The rest of the gpui-kit facade still comes
from crates.io and resolves this `gpui-base` through the patch.

Local changes:

- `input/base/blink_cursor.rs`: only blink an input's caret while it has focus. `pause()` used to
  resume blinking after its delay regardless of focus, so the first programmatic cursor move
  (setting the text) started a timer loop that re-armed itself every 500 ms for the life of the
  input, re-rendering it twice a second with nothing focused (~12 points of idle CPU with a
  competition open in a debug build). The cursor now tracks focus (`start`/`stop`), `pause` is a
  no-op while unfocused, and the loop ends when focus is lost. Has unit tests.

To update: check out the new tag in `vendor/gpui-kit`, re-apply the change, refresh
`vendor/gpui-base-shim/Cargo.toml` from the published crate.

## Runtime switches for the rendering optimizations

The optimizations that live in the vendored gpui / taffy are on by default and each has an
environment variable that turns it off, for comparing against the unoptimized behavior or ruling
one out while debugging. (With every one of them off the debug build is back at its original
~225 ms per scrolled frame; `scripts/perf-stress.sh` measures it.)

| Optimization | Off-switch | Where |
|---|---|---|
| View cache for the sidebar and detail panes (reuse layout, paint and hit-testing of unchanged views) | `DTB_KE_VIEW_CACHE=0` (`off` / `false`) | `crates/dtb-ke-ui/src/app.rs` |
| Retained layout (taffy nodes kept across frames, only changed ones recomputed) | `DTB_KE_LAYOUT_RETAIN=0` | `vendor/zed` `gpui/src/taffy.rs` |
| Wider taffy layout cache (4 entries per slot instead of 1) | `DTB_KE_TAFFY_CACHE_WAYS=1` | `vendor/taffy` `src/tree/cache.rs` |

Debugging aids (off by default): `DTB_KE_VIEW_CACHE_VERIFY=1` re-renders every cached view that
would be reused and logs any difference; `DTB_KE_LAYOUT_VERIFY=1` recomputes every layout from
scratch and compares; `DTB_KE_NO_NATIVE=1` disables the native window backdrop (Liquid Glass /
vibrancy); `DTB_KE_PERF_HUD=0` hides the debug FPS HUD.

## Warnings from vendored code

Cargo hides warnings for registry and git dependencies but not for path dependencies, and every
shim crate here is one. Each shim manifest therefore ends with a `[lints.rust.warnings]` table set
to `allow`, so upstream Zed / taffy / turso / gpui-kit code doesn't add noise to our builds.
`vendor/generate-shims.sh` adds the same table when it regenerates the gpui-pre shims; when
refreshing a shim's `Cargo.toml` from the published crate by hand, re-append it. This only affects
rustc warnings — errors, `cargo:warning` messages from build scripts, and cargo's own
future-incompatibility notices (for example about the registry crate `block`) still show.
