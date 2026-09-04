# Vendored dependency patches

Local, minimally-patched copies of crates.io packages, wired in via the
workspace root `Cargo.toml`'s `[patch.crates-io]` table. Used only when an
upstream bug actively breaks a real build and there's no environment-variable
or configuration escape hatch to work around it from our side — see
RUNNERS.md's "Vendored dependency patches" section for the fuller policy.

Each entry here is:

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

- **`turso_sdk_kit-0.7.2`** — upstream's `build.rs` shells out to a bare
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
