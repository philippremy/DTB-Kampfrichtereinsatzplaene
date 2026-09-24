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

## iOS support (`vendor/zed` PR #63068 + `vendor/gpui-kit` facade patch)

iPadOS runs on the upstream iOS backend, [zed-industries/zed#63068](https://github.com/zed-industries/zed/pull/63068)
(`gpui_ios`, the dispatcher moved into `gpui_apple`, touch/text-input in `gpui`). It is merged into
the `dtb-ke-patches` branch of the `vendor/zed` fork (the merge commit is titled "Merge
zed-industries/zed#63068"), with two local follow-ups: Metal 4 and `renderer_select` stay
macOS-only (iOS uses the Metal 3 renderer), and the Metal 4 renderer skips sprites whose atlas
texture was released (upstream `#64623` made the lookup an `Option`).

- `vendor/gpui-pre-shims/gpui_ios` is a **hand-written** manifest (no published `gpui-pre-ios`
  yet); `gpui_apple`/`gpui_platform` shims gained the iOS target deps. Once the PR lands and gpui-pre
  publishes `gpui_ios`, replace it with the published manifest and drop the merge commit by
  rebasing the fork onto upstream.
- **iPadOS menu bar** (our addition on top of the PR, in the `vendor/zed` fork; not upstream):
  `gpui_ios/src/ios/menu.rs` implements `Platform::set_menus` / `get_menus` / the menu callbacks with
  `UIMenuBuilder`. `AppDelegate` is now a `UIResponder` implementing `buildMenuWithBuilder:`,
  `handleGPUIMenuItem:`, `validateCommand:` and `canPerformAction:withSender:`. The gpui `Menu` model becomes
  `UIMenu`/`UICommand`/`UIKeyCommand`s (separators → inline groups, key equivalents from the keymap, cut/copy/
  paste/select-all → the standard responder selectors); enablement is the same `is_action_available` rule as
  macOS. It replaces the system File/Edit/Format/View/Window/Help menus, and the first gpui menu replaces the
  children of the system application menu. The PR has no hardware-key handling, so these key commands are the
  only path for shortcuts on iPadOS. Drop this when upstream gains its own menu support.
- **Window controls / fullscreen** (ours, `gpui` + `gpui_ios`): `WindowInsets` gains `window_controls`, computed in `gpui_ios`
  from `edgeInsetsForLayoutRegion:` with the corner-adapted safe-area regions (iOS 26; zero before) minus the plain safe
  area, and `PlatformWindow::is_fullscreen` / `is_maximized` report whether the window covers the screen instead of
  always `true`.
- **Document picker** (`gpui_ios/src/ios/documents.rs`, also ours): `export_files` / `pick_files` wrap
  `UIDocumentPickerViewController` so files can leave and enter the app sandbox (iCloud Drive, Downloads, "On My
  iPad", other providers); `Platform::prompt_for_paths` is backed by `pick_files`. There is no `prompt_for_new_path`
  (iOS has no save panel) — callers stage a file in the container and call `export_files`. Needs
  `objc2-uniform-type-identifiers` (added to the shim manifest and the fork's workspace).
- `vendor/gpui-kit-shim` vendors the `gpui-kit` **facade** (`crates/kit` of the gpui-kit submodule).
  Upstream excludes iOS from `gpui_platform` / `gpui_kit::platform` / `application()` and expects a
  downstream `with_platform`; the patch drops that exclusion (Android stays excluded) so iOS is
  handled like every other platform. Drop the shim if upstream does the same.

Check with `cargo check -p dtb-ke-ui --target aarch64-apple-ios-sim` (needs Xcode's iPhoneSimulator SDK;
`gpui_apple`'s build script compiles the Metal shaders with `xcrun -sdk iphonesimulator`).

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
