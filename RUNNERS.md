# CI runners — architecture & setup

This project's Codeberg Actions (Forgejo Actions under the hood) run entirely on
**self-hosted runners** — Codeberg's shared runners are deliberately limited and
this project needs a real macOS box, a real Windows environment, and native
aarch64 cross-compilation, none of which a shared runner offers. This file is
the map: what each machine does, *why* it's built the way it is, and how to
stand one up from scratch. `.forgejo/workflows/tip.yml` and `release.yml` are
the two workflows; `UPDATER.md` covers what the app does with what they
produce; `CLAUDE.md`'s `dtb-ke-bundle` section covers the packaging tool they
both shell out to.

## The machines

| runner label | machine | builds |
|---|---|---|
| `linux-host` | Dell Latitude 7420 (Tiger Lake i7, 32 GB), CachyOS | `x86_64-unknown-linux-gnu` natively, `aarch64-unknown-linux-gnu` cross-compiled (no QEMU); also **hosts the Windows VM** as a libvirt/KVM guest |
| `windows-host` | the Windows VM above | `x86_64-pc-windows-gnullvm`, `aarch64-pc-windows-gnullvm` (cross-linked from the same x86_64 guest) |
| `macos-host` | 16" MacBook Pro, Late 2017, i7 | `aarch64-apple-darwin` + `x86_64-apple-darwin`, merged into one universal binary with `lipo` |

Every job in both workflows targets one of these three labels via `runs-on:
[label]`. There's no fourth physical machine — the Windows runner is a guest
*on* the Linux box, not separate hardware.

## Why each piece is built the way it is

### Windows needs a real Windows environment — a VM, not a container

gpui's Windows backend shells out to **`fxc.exe`**, the classic Direct3D HLSL
shader compiler (Shader Model 5.x / DXBC — the one Direct3D 11 uses), to
build its shaders, not `dxc.exe` (the newer DXIL/Shader Model 6+ compiler
most people mean by "the DirectX Shader Compiler" these days, and the one
that has an official cross-platform/open-source build — an earlier version of
this doc got the two confused). `fxc.exe` has never had a cross-platform
build at all — it's bundled with the Windows SDK and tied to the classic
D3DCompiler component — so this is an even harder Windows-only requirement
than `dxc` would have been. That means Windows builds (and WiX `.msi`
packaging) can only happen where a real Windows SDK is installed and
runnable, which rules out Docker/WSL2-without-a-kernel and any Linux-hosted
cross toolchain entirely.

**Decision: Linux host + Windows VM** (not the reverse). The Linux box is
already carrying the CI orchestration load (`linux-host` builds both Linux
architectures and is the `prepare`/`publish` job runner in both workflows);
adding a headless Windows guest to it is one more VM on a 32 GB box.
Running the reverse (Windows host, Linux guest) would mean redoing the
already-solved aarch64-Linux cross-compilation story inside a VM guest with
less RAM to spare, for no benefit — the Windows leg doesn't need anything the
Linux host can't provide a VM for.

The VM (`scripts/provision-windows-vm.sh`) is headless — no GPU passthrough.
Shader **compilation** is CPU work; nothing here ever renders a frame, so a
software/virtual GPU is fine. **Toolchain: kept as `*-pc-windows-gnullvm`**
(not MSVC) — this was an explicit user requirement from before this CI work
started (see CLAUDE.md), and switching now would touch the crash-handler FFI,
the DOCX font embedding, and the WiX authoring for no upside once the VM
already gives us a genuine Windows environment to run `fxc` in.

**WiX needs CET disabled for its own process, guest-side.** A real build hit
WiX's .NET-built `wix.exe` apphost (CET-shadow-stack-marked by the SDK that
built it) refusing to start with *"Your System does not fully support
CET"* — even with `--cpu host-passthrough` (the Latitude's Tiger Lake CPU
genuinely supports CET). That phrasing — **fully** — was the tell: not a CPU
that lacks the feature, but a guest that sees the CPUID bit (from
passthrough) and fails a deeper runtime completeness check, because KVM's
virtualization of CET's actual machinery (the shadow-stack MSRs, not just
the CPUID leaf) is a much newer, less mature area than plain CPUID
passthrough. A hypervisor-side fix (stripping the CET feature bits from the
passed-through CPU model, `--cpu host-passthrough,-shstk,-ibt`) was tried
first and **turned out to be unnecessary** — the fix that actually worked is
entirely inside the guest, a per-process mitigation override:

```powershell
Set-ProcessMitigation -Name wix.exe -Disable UserShadowStack
```

This writes a persistent, machine-wide registry policy
(`HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\Image File Execution
Options\wix.exe`) that Windows' process loader consults for *any* launch of
an executable named `wix.exe`, regardless of what starts it — an interactive
shell, the runner service, a CI job — so once set, it covers every future
build without needing to be reapplied per-run. Applied in two places for
durability: `runner-setup-windows.ps1` sets it (idempotently) right after
installing WiX, so a from-scratch or reprovisioned VM gets it automatically;
each Windows CI job also re-asserts it as an early, cheap, idempotent step
before building, as a defensive belt-and-suspenders against the registry
policy ever being reset out of band (a Windows Update, a snapshot rollback,
a VM rebuilt without re-running the full setup script).

**WiX v6+ gates every subcommand behind the OSMF EULA.** `dotnet tool install
wix` with no version pin now pulls WiX v7, which refuses to run *any*
subcommand — `build`, `extension add`, `extension list` — with `error WIX7015:
You must accept the Open Source Maintenance Fee (OSMF) EULA` until it's
accepted. Two independent acceptances, because the CI `wix build` runs as
LocalSystem (the runner service) while setup runs as the interactive user, and
`wix eula accept` records acceptance *per profile*: (1) `runner-setup-windows.ps1`
runs `wix eula accept wix<major>` right after install so the setup script's own
`wix extension add -g` works; (2) `dtb-ke-bundle`'s `windows.rs` passes
`-acceptEula wix<major>` inline on its `wix build` invocation (`wix_major()`
reads `wix --version`, only adds the flag for v6+) — a one-off, profile-independent
acceptance that covers the LocalSystem CI run. The `.wxs` we emit is still v4
schema (`schemas/v4/wxs`), which v7 reads fine.

### Linux aarch64: cross-compiled, no QEMU

QEMU-emulated aarch64 builds were tried on this hardware before and were,
in the user's words, "horrendously slow" — rejected outright. Instead:
**native cross-compilation** from the x86_64 CachyOS host, with an aarch64
sysroot assembled by **unpacking real Debian `.deb` files** (never executing
foreign-arch code — `dpkg-deb -x` just extracts an archive).

This only works because the dependency tree was actually audited, not
assumed clean:

- `cargo tree --target aarch64-unknown-linux-gnu -i <crate>` was run against
  every `*-sys` / build-helper crate in `Cargo.lock` to check whether it's
  **actually** in the resolved graph for that target and with these features
  — several turned out to be for other platforms or optional features and
  don't matter (`aws-lc-sys`, anything pulling in `cmake`, none of which are
  reachable here).
- The real cross-link risk is four crates that build/link against system C
  libraries: **`freetype-sys`**, **`yeslogic-fontconfig-sys`**,
  **`wayland-sys`**, **`khronos-egl`** (all from font-kit/gpui's Linux
  backend). Their build scripts use `pkg-config`, which — unlike a plain
  `cc` invocation — actively searches and links against whatever it finds,
  so it has to be pointed at the aarch64 sysroot specifically
  (`PKG_CONFIG_ALLOW_CROSS=1`, `PKG_CONFIG_SYSROOT_DIR`, `PKG_CONFIG_PATH`
  scoped under the sysroot — see the `linux-arm64` job's `env:` block in
  both workflow files). Nothing else in the tree links against a system
  library; a plain `cc` (several build scripts use one) is harmless because
  it doesn't link anything by default.
- **The final link step needs `-L`, not `--sysroot`.** `RUSTFLAGS` passes
  `-C link-arg=-L<sysroot>/usr/lib/aarch64-linux-gnu -C link-arg=-L<sysroot>/
  lib/aarch64-linux-gnu` (the two multiarch paths our extracted `.deb`s'
  libraries actually landed in — Debian splits `usr/lib/<triplet>` vs.
  `lib/<triplet>` per-package) — deliberately **not** `--sysroot=<our
  sysroot>`, which was the first attempt and broke a real CI run: Arch's
  `aarch64-linux-gnu-glibc` package ships glibc under gcc's own baked-in
  default sysroot (`aarch64-linux-gnu-gcc -print-sysroot` → `/usr/aarch64-
  linux-gnu`, confirmed against a real runner — `pacman -Ql aarch64-linux-
  gnu-glibc` shows `libm.so.6` living exactly there), and passing `--sysroot`
  *replaces* that default wholesale rather than adding to it. The failure
  mode was non-obvious: `ld: cannot find /lib/libm.so.6: file in wrong
  format` — glibc resolution silently fell through to the host's own
  (x86_64) `/lib` instead of erroring on a missing sysroot. `-L` is additive,
  so gcc's own correct default keeps handling glibc while our sysroot only
  supplies the four GUI libraries above.
- The four cross-link crates weren't the whole story — the first pass of this audit missed gpui_linux's
  **X11/XCB backend** (`xcb`, `xkbcommon`, `xkbcommon-x11`) entirely, which only surfaced once a real
  build got far enough to actually fail on it (`cannot find -lxcb` etc.). And two of the packages that
  *were* identified had the wrong `-dev` name: **`libfontconfig-dev`/`libfreetype-dev`, not
  `libfontconfig1-dev`/`libfreetype6-dev`** — verified by pulling the real `.deb`s and inspecting their
  contents directly (not guessed): the `1`/`6`-suffixed packages are empty Debian transitional packages
  (changelog + copyright only), while the unsuffixed ones are what actually ships
  `usr/lib/aarch64-linux-gnu/libfontconfig.so` — the linker needs that exact unversioned symlink for a
  bare `-lfontconfig`, and the versioned `.so.1` runtime file alone isn't enough. Lesson: a `.deb`
  resolving and extracting cleanly proves nothing about whether it's the *right* package — when a link
  step names a missing `-l`, pull the specific `.deb` and check its contents before assuming the
  package list just needs an addition.
- **`-L` finds a library the linker is told to look for directly; it does not resolve *that*
  library's own further dependencies.** `libfontconfig.so` itself needs `libexpat.so.1`;
  `libxcb.so` needs `libXau.so.6`/`libXdmcp.so.6`; `libxkbcommon-x11.so` needs `libxcb-xkb.so.1`;
  `libEGL.so` pulls in the Mesa/DRI/GBM stack. GNU ld only searches `-L` paths for the libraries
  named directly (`-lfontconfig` etc.); resolving a shared library's own `DT_NEEDED` chain at link
  time needs **`-Wl,-rpath-link`** — confirmed against a real failure where ld warned
  `libexpat.so.1, needed by libfontconfig.so, not found` even with `libexpat.so.1` sitting right
  there on an `-L` path, then failed for real with `undefined reference to XML_SetUserData` and
  dozens more. Both flags are needed together (`RUSTFLAGS` carries `-L` *and* `-Wl,-rpath-link` for
  both multiarch sysroot dirs).
- **The package list needs the full transitive runtime closure, not just what's directly linked.**
  Rather than keep discovering the next missing `.so` one broken CI run at a time, the eventual
  fix resolved it programmatically: a small Python BFS over the live Debian index's `Depends:`
  fields, seeded from the directly-linked packages, first alternative of each `Depends:` taken,
  filtered to `lib*` packages and excluding the ones that come from the host's own glibc/gcc
  (`libc6`/`libgcc-s1`/`libstdc++6`). That's what `runner-setup-linux.sh`'s `PACKAGES` array now is
  — roughly 30 packages, most of them (Mesa/DRI/GBM/X11-extension libraries) never touched directly
  by name anywhere in this codebase, present purely because something we *do* link against needs
  them transitively.
- `scripts/runner-setup-linux.sh` resolves the exact `.deb` filenames/versions
  it needs **live against Debian's package index** (`dists/bookworm/main/
  binary-arm64/Packages.xz`, parsed in `awk` paragraph mode) rather than
  hardcoding version strings that go stale — verified in this project's setup
  session by actually fetching the index and downloading all 13 real `.deb`s.
- **Packaging the arm64 leg.** `cargo dtb-ke-bundle bundle --target
  aarch64-unknown-linux-gnu` threads that triple into `bundle::Context`;
  `linux::arch_labels` maps its arch token to the Debian (`arm64`) and
  RPM/AppImage/tar (`aarch64`) spellings so the `.deb`/`.rpm`/`.AppImage`/tarball
  are all correctly labelled around the arm64 binary — an unmapped arch is a
  hard error rather than a silent `amd64`-labelled arm64 package. `rpmbuild` is
  invoked with **`--target <arch>`** (bare CPU — `rpm-common(8)` says PLATFORM
  is `arch[-os]`, os optional) — without it rpm fails the aarch64 leg with `No
  compatible architectures found for build` because the build host is x86_64;
  `--target` makes rpm use that arch's config instead of autodetecting, and is
  the *only* thing setting the package arch (the generated spec has **no
  `BuildArch`** — `rpmbuild(1)` BUGS: mixing `--target` with a spec `BuildArch`
  is undefined behavior). No foreign code runs — it's just an archive +
  scriptlets.

### macOS: universal binary via two sequential `lipo` slices

`cargo dtb-ke-bundle bundle --release --universal` builds
`x86_64-apple-darwin` and `aarch64-apple-darwin` **sequentially, never in
parallel** — each slice needs its own architecture-matched embedded crash
helper staged first (`dtb-ke-crash`'s build script stages whatever was most
recently built; a mismatched-arch helper fails to `execve` at all), so the
two builds can't share a `target/` at the same time. The two `.o`/binary
slices are then merged with `lipo -create`. Release builds are **Developer-ID
signed and notarized** when `MACOS_SIGN_IDENTITY` is set (below); Tip builds,
and a Release build cut without that secret, stay ad-hoc signed.

**The macOS bundle's main executable is named `meta::MACOS_EXECUTABLE_NAME`
(`DTB-Kampfrichtereinsatzplaene`, an ASCII transliteration), not the branded
`meta::DISPLAY_NAME`.** `codesign` on macOS 26 cannot ad-hoc-sign an
app bundle whose main executable's *filename* contains a non-ASCII character
(the "ä" in "Kampfrichtereinsatzpläne"): `codesign --sign` fails outright
with `code object is not signed at all / In subcomponent: …/MacOS/<name>`,
and even a bundle that does get signed fails `codesign --verify --strict`
with `a sealed resource is missing or invalid`. Reproduced and narrowed
locally against this exact binary — `DTB App` (ASCII + space) signs and
verifies fine, `DTB Kampfrichtereinsatzpläne` does not; an ASCII executable
name fixes both. The bundle *directory* keeps the branded name (an "ä" there
is fine), and so do `CFBundleName` / `CFBundleDisplayName` — Finder, the menu
bar, the Dock and Force-Quit all read those, so the only user-visible change
is the process name in Activity Monitor / `ps`. (The per-slice signature
state — the native `aarch64-apple-darwin` slice comes out linker-signed
ad-hoc, the cross-linked `x86_64-apple-darwin` one unsigned — is *not* the
problem: signing tolerates that mismatch fine once the executable name is
ASCII.)

### macOS SDK restamp (Tahoe interface)

macOS 26 ("Tahoe") gates its redesigned interface — larger traffic-light
window controls, Liquid Glass chrome — on the **SDK version the binary was
linked against**, recorded in the Mach-O `LC_BUILD_VERSION` load command's
`sdk` field and read by AppKit via `dyld_program_sdk_at_least`. It is *not*
a runtime-OS check: a binary linked against an SDK < 26 gets the
pre-redesign look even when run on Tahoe.

The `macos-host` runner is a 2017 Intel MacBook Pro — Ventura is its last
supported macOS, so its linker can only stamp the **13.x** SDK, and Xcode 26
(needed for the 26 SDK) requires macOS 15.5+. There is no Info.plist opt-in
to the new look from an old SDK (`UIDesignRequiresCompatibility` only forces
the *old* look with a *new* SDK).

So `dtb-ke-bundle`'s macOS packaging (`macos::ensure_min_sdk`) restamps the
field with **`vtool`** right after copying the binary into the `.app` and
before `codesign` (so the change is inside the signed bundle):

```sh
vtool -show-build <exe>                                  # read current sdk
vtool -set-build-version macos 11.0 26.0 -replace -output <exe> <exe>
```

- Floor is `meta::MACOS_SDK_FLOOR` (`"26.0"`); deployment target
  (`minos` / `LSMinimumSystemVersion`) stays `meta::MACOS_MIN_VERSION`
  (`"11.0"`).
- Never lowers: a build *on* macOS 26 (SDK ≥ floor, e.g. a local
  `cargo dtb-ke-bundle bundle`) is left untouched.
- `vtool` handles a universal binary directly — every slice is rewritten.
  It drops the `LC_BUILD_VERSION` tool sub-entries (`ntools 0`), which are
  informational only.
- `vtool` ships with the Command Line Tools (`/usr/bin/vtool`, same shim as
  `lipo`) — no extra runner setup.

**Faking the SDK means AppKit enables *every* 26-SDK-gated behavior** on a
binary compiled without the 26 headers — for this app that surface is small
(gpui draws its own UI; the exceptions are `NSSavePanel` in `src/save/` and
the native menu bar), but a real smoke test on both Ventura and Tahoe is
warranted after the first CI-built release. If the newer Mac ever becomes
the `macos-host` runner, this step becomes a no-op and can be dropped.

## The two workflows

`.forgejo/workflows/tip.yml` — every push to `main`. Builds all five targets
(`linux-x86_64`, `linux-arm64`, `windows-x86_64`, `windows-arm64`,
`macos-universal`) as fast as possible: **LTO disabled, more codegen units,
incremental compilation on** (`CARGO_PROFILE_RELEASE_LTO=false`,
`CARGO_PROFILE_RELEASE_CODEGEN_UNITS=16`, `CARGO_INCREMENTAL=1`), and —
because these are **persistent, not ephemeral, self-hosted runners** —
`target/` is simply never wiped between Tip runs, so every push after the
first gets real incremental-build speedup for free. Produces one rolling
release under the `tip` tag (force-moved to the new commit each run,
`--prerelease`), versioned `X.Y.(Z+1)-dev.N` where `N = git rev-list --count
--first-parent vX.Y.Z..HEAD` (commits since the last release — monotonic,
resets per release, needs the `prepare` job's `fetch-depth: 0`). `self_update`
orders digit-only prerelease identifiers numerically, so `dev.7 < dev.12`.
Full scheme in **UPDATER.md § 3**.

`.forgejo/workflows/release.yml` — triggered by a `vX.Y.Z` tag push (only
`scripts/release.sh` should ever create one) or manual `workflow_dispatch`.
Every job starts with `rm -rf target` — a clean build with the workspace's
normal profile (fat LTO, `codegen-units = 1`), trading speed for the smallest
possible binary (see CLAUDE.md's binary-size notes). Adds the installer
formats Tip skips (`.msi` via WiX, `.deb`/`.rpm`/`.AppImage`), zipsign-signs
every archive, and — macOS only — signs Developer-ID and notarizes when
`MACOS_SIGN_IDENTITY` is set, or falls back to ad-hoc (skipping the
`Notarize` step, gated on the same secret) when it isn't: a small user base
means ad-hoc-everywhere is an accepted trade-off here, unsigned is the only
thing that's actually disallowed. Publishes to **both** the version tag's own
release and a rolling `latest` release (force-moved the same way `tip` is) —
`latest` is the stable URL the app's updater actually polls
(`MANIFEST_URL_STABLE` in `crates/dtb-ke-ui/src/updater/mod.rs`).

Both workflows follow the same job shape: a `prepare` job creates the
Codeberg release(s) via `cargo dtb-ke-bundle codeberg prepare` and outputs
the `release_id`(s); the per-arch build jobs run **in parallel**, each
independently uploading its own archive straight to those release(s) via
`cargo dtb-ke-bundle codeberg upload` — there's no shared filesystem or
artifact-passing action between runners, the release's own asset list *is*
the synchronization point. Each upload also drops a small `<name>.fragment.json`
sidecar (name/url/size/digest) alongside the real archive; a final `publish`
job downloads every fragment, merges them into the real `manifest.json`
(`cargo dtb-ke-bundle codeberg manifest-publish`), uploads that, deletes the
fragments, and un-drafts the release.

Note that `codeberg prepare` **never deletes or retargets a git tag** — only
the release object. For a real version tag that's correct because
`release.sh` already put the tag exactly where it should be; for the rolling
tags (`tip`, `latest`) the workflow force-moves them itself
(`git tag -f … && git push -f …`, using a token-embedded remote URL) *before*
calling `codeberg prepare`, since the release-create API can create a
brand-new tag but never retarget an existing one.

## Forgejo Actions schema gotchas (found by Codeberg's real validator)

YAML syntax validity (checked locally with PyYAML while writing these files)
is necessary but not sufficient — Codeberg's own "Workflow config file is
invalid" validator checks against the actual Forgejo Actions schema and
caught two real bugs neither PyYAML nor careful reading turned up. Both are
fixed in the current files; noted here so a future edit doesn't reintroduce
them:

- **There is no `gitea` context.** Forgejo's real top-level contexts are
  `forgejo` (or `forge`), `github` (an alias of `forgejo`, kept for
  GitHub-Actions-compatible expressions — this is the one to use), `secrets`,
  `vars`, `env`, `matrix`, `steps`, `needs`, `inputs`, `runner`. An expression
  like `${{ gitea.sha }}` fails schema validation, and the error it produces
  is a cascade of confusing, seemingly-unrelated "Unknown Property" messages
  at the *job* and *step* level (`outputs`, `steps`) rather than pointing at
  the actual bad expression — the real culprit is always the deepest
  "Unknown Variable/Property Access" line in the error output.
- **`workflow_dispatch` inputs require an explicit `type:` field** (`string`,
  `boolean`, `number`, or `choice` with an `options` list) — GitHub Actions
  treats it as optional and defaults to `string`; Forgejo's schema does not.
  Omitting it doesn't produce a targeted error at all — the whole file fails
  to build a valid job graph and Codeberg reports the generic **"The
  workflow must at least contain one job without dependencies"**, even when
  every job's `needs:` is written correctly. If that message shows up despite
  an obviously-correct job graph, suspect the `on:` block, not `jobs:`.
- Relatedly: reference a `workflow_dispatch` input as **`inputs.<name>`**
  (its own top-level context), not `github.event.inputs.<name>` — the docs
  list `inputs` as a first-class context, not something nested under
  `github`/`forgejo`.
- `needs:` on a job is written as a YAML block list (`needs:` / `- job`) in
  every documented example; a flow-style `needs: [a, b, c]` wasn't confirmed
  to fail, but isn't confirmed to work either — both workflow files now use
  the documented block-list form throughout rather than relying on that.

Source: `https://forgejo.org/docs/latest/user/actions/reference/`.

## Target-dir caching

"Persistent, self-hosted runners" doesn't by itself make Tip's incremental
caching work. Forgejo's host-execution runner checks each **job** out into a
fresh, randomly-named directory (`$HOME/.cache/act/<rng>/…` on Linux/macOS,
analogous on Windows) — a new one every run, on every job, even on a runner
that itself never gets rebuilt. A plain `cargo build` inside that checkout
writes to `./target`, which is gone the moment the job ends; the *runner*
persists, but nothing under its checkout does. Left alone, this makes
`tip.yml`'s "efficient incremental caching" claim a no-op — every Tip build
would compile from scratch, indistinguishable from `release.yml`'s
intentionally-clean builds.

The fix: every Tip job sets **`CARGO_TARGET_DIR`** to a fixed, absolute path
*outside* the checkout, pre-created (and, on Linux, chowned to the `runner`
user) once by the relevant setup script:

| runner | cache root | created by |
|---|---|---|
| `linux-host` | `/var/cache/Codeberg/DTB-Kampfrichtereinsatzplaene/` | `runner-setup-linux.sh` |
| `windows-host` | `C:\Codeberg\DTB-Kampfrichtereinsatzplaene\` | `runner-setup-windows.ps1` |
| `macos-host` | `/Users/Shared/Codeberg/DTB-Kampfrichtereinsatzplaene/` | `runner-setup-macos.sh` (this exact `/Users/Shared/Codeberg/<repo>/Cache` shape carries over from the user's own prior experience with this setup) |

Under each root, `tip.yml` further scopes every job to
`Cache/tip/<triple>` — the **`tip/`** segment keeps a concurrently-running
Release build (which shares the same physical machine and label but a
different workflow) from ever colliding with Tip's cache, and the
**per-triple** segment lets the two parallel per-arch jobs on the same host
(`linux-x86_64`/`linux-arm64`; `windows-x86_64`/`windows-arm64`) build at the
same time without serializing against each other's `cargo` lock file. The
`macos-universal` job needs only one such directory (`.../tip/
universal-apple-darwin`) since `--universal` already builds both
`apple-darwin` slices *sequentially* under one shared target dir — cargo
itself segregates them into `<dir>/<triple>/release/` when `--target` is
passed, so no further splitting is needed there.

This path is written with backslashes, not forward slashes, in the workflow
YAML — an earlier version used `C:/Codeberg/…` (forward slashes, to avoid
YAML/shell backslash-escaping; a plain unquoted YAML scalar preserves
backslashes literally too, so escaping was never actually required either
way). Backslashes are simply the correct native form; **they turned out not
to be the fix for the `fxc.exe` failures this path was first suspected of
causing** — that was a real bug, but a different one, in `fxc.exe`
discovery itself (see the `find_fxc_compiler` note in the Windows section
above), confirmed by actually reading `gpui_windows`'s `build.rs` rather
than guessing from the error text a second time.

`release.yml` deliberately does **not** set `CARGO_TARGET_DIR` at all. Every
release job's `rm -rf target` already operates correctly against the
checkout-local, ephemeral `./target` — and since each job gets a *fresh*
checkout from Forgejo regardless, there is no staleness for a fixed cache
path to solve there; adding one would only add cache-directory bookkeeping
release builds get no benefit from.

`dtb-ke-bundle` itself has to agree with cargo about where the build landed:
`util::target_dir`/`target_dir_for` (used to locate the just-built binary
and to stage the crash helper) read `$CARGO_TARGET_DIR` the same way cargo
does — absolute as given, a relative value resolved against the workspace
root — falling back to the workspace's own `target/` when unset (every local
dev build). The *packaging* output (`target/bundle/<profile>/`, what the
workflow `tar`s up and uploads) deliberately does **not** follow
`CARGO_TARGET_DIR` — it stays checkout-local and disposable, which is
correct: it's cheap to regenerate and uploading it is the whole point of the
job, so there's nothing worth caching there.

## Update channels

`Settings.update_channel` (`Stable` / `Tip`) picks which manifest URL the
in-app updater polls — see `crates/dtb-ke-ui/src/settings.rs` and
`updater/mod.rs`. `updater::can_self_install(channel)` is `false` for
macOS+Tip (Tip archives are only ad-hoc signed, so an in-app Gatekeeper-clean
relaunch isn't reliable there — Tip users on macOS get a "Herunterladen" link
instead of an in-app install, same as the Linux path always does).

## Setup, in order

1. **Linux box** (`scripts/runner-setup-linux.sh`): Rust + aarch64 target,
   the aarch64 cross toolchain + sysroot, Linux packaging tools
   (`.deb`/`.rpm`/AppImage), zipsign, libvirt/QEMU (to host the Windows VM
   next), the `forgejo-runner` binary (prebuilt Linux release exists), and
   its registration + systemd service. Idempotent — re-run any time.
2. **Windows VM** (`scripts/provision-windows-vm.sh`, run *on* the Linux
   box once step 1's libvirt install is done): creates the headless
   `virt-install` guest. You still do the actual Windows install yourself
   over VNC (`virsh domdisplay`) — there's no unattended/scriptable way to
   get a Windows Server 2022 Evaluation ISO without going through
   Microsoft's EULA-gated download form first.
3. **Windows VM guest** (`scripts/runner-setup-windows.ps1`, run *inside*
   the VM once Windows is installed and reachable): Rust +
   `*-pc-windows-gnullvm` targets, llvm-mingw (the gnullvm cross toolchain),
   the Windows SDK (for `fxc.exe` — see below), the WiX Toolset CLI (a
   `dotnet tool` — currently v7; its OSMF EULA is auto-accepted, see the
   WIX7015 note above), Go + a from-source `forgejo-runner` build (**no Windows
   binary is published upstream** — the v13.1.0 release only ships Linux
   images; building it needs only Go, per Forgejo's own docs), zipsign, and
   registration.

   The Windows SDK step (installing it standalone) has needed two real
   fixes, both confirmed against actual build failures rather than guessed —
   the second one specifically by pulling up `gpui_windows`'s real
   `build.rs` (from the exact pinned commit in `Cargo.lock` — a local
   `~/.cargo/git/checkouts/zed-<hash>/<rev>` had it already fetched) and
   reading `find_fxc_compiler()` rather than continuing to guess from error
   text alone:
   - The SDK ships `fxc.exe` under separate `x86`/`x64`/`arm64`
     subdirectories, and a bare `-Recurse` search with no architecture
     filter picked up a mismatched copy — Windows error 216
     (`ERROR_EXE_MACHINE_TYPE_MISMATCH`) the first time this was hit. Fixed
     by filtering for the `x64` copy specifically (this VM is x86_64,
     whether building `x86_64-pc-windows-gnullvm` natively or cross-building
     `aarch64-pc-windows-gnullvm` — a build-script host tool always needs
     the *host's* architecture, independent of the Rust target).
   - `find_fxc_compiler()`'s PATH-based fallback runs `where.exe fxc.exe`
     and does `.trim()` on the **entire** output as if it's always a single
     path — but `where.exe` prints one match per line when a command exists
     in more than one `PATH` directory, and `Set-MachinePath` only ever
     *prepends* (it never removes a stale entry an earlier, unfiltered run
     of this script had already added). With two `fxc.exe` directories on
     `PATH`, `where.exe` printed two lines, `.trim()` didn't collapse them,
     and the resulting multi-line blob isn't a valid path at all —
     `Command::new(fxc_path).output()` failed at the `CreateProcess` level
     (`Err(e)`, not a nonzero exit from `fxc.exe` actually running), which
     is why the error was a raw OS error (123, `ERROR_INVALID_NAME`) rather
     than anything from `fxc.exe` itself. Sidestepped rather than chased —
     `find_fxc_compiler()` checks a `GPUI_FXC_PATH` env var *first*, before
     ever calling `where.exe`, so the script now sets that to the exact
     resolved x64 path (persistent Machine scope), which wins outright
     regardless of how polluted `PATH` still is.

   **There is no bash on this VM, and nothing in this setup installs one.**
   Git for Windows (which bundles Git Bash) is never installed anywhere in
   `runner-setup-windows.ps1` — `actions/checkout@v6` and the `git clone`
   for building `forgejo-runner` both need real `git`, but that's a
   separate thing from Git Bash being present or on `PATH`. Every
   `windows-x86_64`/`windows-arm64` job step that packages and uploads a
   build is native PowerShell (the runner's default shell here, same as the
   `Build` steps) — **not** `shell: bash` — for exactly this reason; an
   earlier version of both workflow files used `shell: bash` throughout and
   every one of those steps failed outright.
4. **macOS box** (`scripts/runner-setup-macos.sh`): Xcode Command Line
   Tools, Rust + both `apple-darwin` targets, Go + a from-source
   `forgejo-runner` build (same reasoning as Windows — no macOS binary
   ships), zipsign, code-signing/notarization credential guidance, and
   registration as a `launchd` agent. Also nags about disabling sleep — a
   CI box that goes to sleep just hangs scheduled runs.

Each script is **idempotent**: check-then-prompt-then-install, safe to
re-run after an interrupted step or to pick up a new tool later. None of
them assume network access to anything but the tool's own official
download/package source and Codeberg itself.

## Registering a runner

Codeberg → this repo → **Settings → Actions → Runners → Create new Runner**
generates a **UUID + registration token pair** and shows the exact command
to run. There is no separate `forgejo-runner register` step any more (that
subcommand and its `.runner` state file are the *old* flow) — `forgejo-runner
daemon` takes the connection info directly:

```sh
echo -n "<TOKEN>" > /path/to/runner-token
forgejo-runner daemon \
  --url https://codeberg.org/ \
  --uuid <UUID> \
  --token-url file:///path/to/runner-token \
  --label <LABEL>
```

The token goes in a file (`--token-url file://…`), not inline on the command
line. Each setup script prompts for the instance URL, UUID, and token,
writes the token to a file (`chmod 600`), and bakes the resulting `daemon`
invocation into the platform service definition (systemd unit / launchd
plist / NSSM). The **`--label`** value is what `runs-on: [label]` in the
workflow files matches against — it must be exactly `linux-host:host`,
`windows-host:host`, or `macos-host:host` (the `:host` suffix is the
execution mode; see the table above for which machine is which) for the
workflows to find the right runner.

**Registration tokens are single-use and short-lived.** If a `daemon`
attempt with one fails for *any* reason — a typo, a script bug, a network
hiccup — that token is burned; retrying with the same value fails with
`invalid_argument: runner registration token not found`, not a repeat of
the original error. Always get a fresh UUID+token pair from Codeberg before
retrying, and only enable/start the service once you've watched a `daemon`
run actually connect successfully (each script's registration step writes
the token file and service definition together, but doesn't itself verify a
successful connection — check the service's logs after enabling it).

To re-register (e.g. after rebuilding a machine): delete the runner's token
file and service definition (paths are printed by each script) and re-run
the script's registration step with a fresh UUID+token pair.

## Portability: no arbitrary CLI tool dependencies

`dtb-ke-bundle` runs unattended on all three CI hosts — Linux, Windows,
*and* macOS — so it must never shell out to a tool whose presence is merely
assumed rather than guaranteed. This was violated twice, both caught by
real CI failures rather than review:

- **`manifest::sha256`** shelled to `sha256sum` (Linux) or `shasum -a 256`
  (macOS) to hash a release archive for the manifest/upload fragment —
  failed outright on Windows (`program not found`), since neither exists
  there. Replaced with the `sha2` crate, computed in pure Rust.
- **`codeberg::Client::upload_raw`** shells to `curl -F` for the
  multipart-form-data asset upload — **tried and reverted a pure-Rust
  replacement here, twice.** A hand-rolled one-part `multipart/form-data`
  body sent via `ureq` was tried first (avoiding the same category of
  assumption that had just broken for `sha256sum`/`shasum` — "present by
  default on Windows 10 1803+", true but not a pattern worth repeating).
  It failed a real upload on both Linux (`Broken pipe`) and Windows
  (`connection … aborted`) — the server closing the connection mid-write,
  on both platforms, for a request `curl` had just handled fine. Diagnosed
  (by reading `ureq`/`ureq-proto`'s actual source, not guessing) that `ureq`
  supports `Expect: 100-continue` but doesn't send it automatically the way
  `curl` does; added the header — and the *next* real upload failed
  differently, an outright HTTP 400. Two failures against a live service
  with no local way to reproduce either one is enough: reverted to `curl`
  on the user's explicit instruction rather than keep debugging blind.
  `curl`'s presence on all three CI hosts is at this point an *empirically
  confirmed* fact (it has uploaded real archives successfully many times
  over the course of this project's CI work), not the kind of unverified
  assumption the `sha256sum`/`bash` failures were — so this one exception
  to the portability rule above stands, deliberately, not as an oversight.

  **The `curl` revert then hit its own real bug, Windows-only, and it
  wasn't a `curl`-vs-`ureq` question at all.** This project's `DISPLAY_NAME`
  (and so every archive's asset name) contains "ä", and `upload_raw` put
  that name into a URL query parameter (`?name=…`) and a `curl -F …
  ;filename=…` field as raw, un-encoded text — Linux uploaded such a file
  fine, Windows failed every one with `curl: (22) … 400`. Root cause,
  reasoned from how Windows actually passes command-line arguments (not
  guessed from the error text alone): `curl.exe`'s ANSI-CRT entry point
  converts the process's wide (`CreateProcessW`) command line down to a
  narrow one using the system ANSI code page. That round-trip is lossless
  for the **local file path** — the same code page converts it back to wide
  on the `CreateFileA` side, so the upload always read the right bytes off
  disk, matching the observation that this got as far as a real HTTP
  response rather than failing to find the file. But the **query value and
  the `Content-Disposition: filename=` text are just bytes sent over the
  wire**, with no second conversion to undo the first — "ä" (UTF-8 `C3 A4`)
  arrived as the single mangled byte `E4`, invalid UTF-8, which Codeberg
  correctly rejected with 400. Fixed by percent-encoding the asset name
  (RFC 3986 unreserved set, hand-rolled — a dozen lines, no new dependency)
  before it becomes a `curl` argument at all: pure ASCII text can't be
  reinterpreted by any code page on either platform, and it's also simply
  the standards-correct way to put arbitrary text in a URL query parameter.
  See `url_encode` in `codeberg.rs`.

**The standing rule going forward**: a shelled external command in
`dtb-ke-bundle` is only acceptable when it is a genuinely platform-native
tool with no practical portable alternative (`codesign`, `wix`, `iconutil`,
`actool`, `lipo`, `rpmbuild`, `appimagetool`) **and** the call site only ever
executes on that tool's own platform (gated by a `cfg!(target_os = …)` check,
the same way `icon.rs::generate` refuses to run `actool`/`iconutil` outright
on non-macOS hosts — the `icons` command is macOS-only full stop; see its
module doc). Anything invoked from code that can run on *any* of the three CI
hosts — which includes all of `codeberg.rs` and `manifest.rs`, since every
job in every workflow calls into them — gets a real Rust dependency
instead, even when the shelled-out tool would probably have been present.
`cargo` itself is the one standing exception, for the obvious reason.

`codeberg::Client::upload_raw`'s `curl` call is a **second, deliberate**
exception, not a gap in this rule: it isn't an unexamined assumption like
`sha256sum`/`bash` were, it's the result of trying the portable Rust
replacement twice, watching it fail two different ways against the real
service, and reverting on explicit instruction. Don't re-attempt a pure-Rust
multipart upload here without a way to actually test it against Codeberg
first — a third silent regression on a real release is worse than the
`curl` dependency.

**PowerShell string-splat trap.** The Windows jobs build the upload arg list
in PowerShell. `$x = (Get-ChildItem …).FullName` is a *scalar string* when the
glob matches one file (and `$null` when it matches none); splatting either
with `@x` in Windows PowerShell 5.1 does *not* pass one path argument — it
enumerates the string's characters, so `codeberg upload` received `C`, `:`,
`\`, … and died on `\` (`bad asset path: \` — a lone backslash is the current
drive root, so it survives the `is_file()` check but has no file name). Fix:
`@("$name") + @(Get-ChildItem … -File | ForEach-Object FullName)` — `@(…)`
forces a real array (empty or not), and splatting an *array* passes one arg
per element. `run_codeberg`'s arg filter also now skips any positional that
isn't `is_file()` (with a warning), so a stray arg can't crash the upload.

## Vendored dependency patches

`/vendor/` (see `vendor/README.md`) holds local, minimally-patched copies of
crates.io packages wired in via the workspace root `Cargo.toml`'s
`[patch.crates-io]` table — used only when an upstream bug actively breaks a
real build and there's no environment-variable or configuration escape
hatch to work around it from our side (checked for, not assumed — see the
`turso_sdk_kit` entry below for what that verification looked like). Not a
general-purpose fork-and-maintain mechanism: each entry is the *exact*
published source for the version already in `Cargo.lock`, with the smallest
possible targeted fix, commented at the change site, removed the moment a
real fix ships upstream.

**`turso_sdk_kit` 0.7.2** — the aarch64 Windows cross-build failed with
`ld.lld: error: ...turso_sdk_kit_version.res: machine type x64 conflicts
with arm64`. Rather than guess at a workaround, fetched the *exact*
published source for the pinned version directly from crates.io
(`api/v1/crates/turso_sdk_kit/0.7.2/download` — needs a `User-Agent` header
or crates.io 403s) and read the real `build.rs`: it shells out to a bare
`windres` (Command::new("windres")) with **no** `CARGO_CFG_TARGET_ARCH`
check and no target-triple prefix anywhere in the file — confirmed by
grepping the actual downloaded source, not the GitHub `main` branch (a
published crate isn't guaranteed to match a monorepo's current `HEAD`).
That resolves to whichever architecture's copy of `windres` happens to be
first on `PATH` — correct by coincidence for the native x86_64 build,
wrong for the aarch64 cross-build sharing the same `PATH`. No env var
escape hatch exists to redirect it (checked — `WINDRES`, anything
target-arch-aware, nothing). Vendored as the `vendor/turso` git submodule (fork `philippremy/turso`, branch `dtb-ke-patches`, based on the exact `046e9cb` commit 0.7.2 was published from) via `vendor/turso/sdk-kit` directly (the fork's branch carries the published `Cargo.toml`),
patched to build `<CARGO_CFG_TARGET_ARCH>-w64-mingw32-windres` (the
llvm-mingw/mingw-w64 target-prefixed name — the same naming convention this
project's own Windows build already depends on, see `dtb-ke-ui`'s
`build.rs` in CLAUDE.md) instead of the ambiguous bare name. Verified
locally (on macOS, where the crate's own build script no-ops immediately
since it only does anything under `CARGO_CFG_WINDOWS`) that the patch
actually takes effect: `cargo check -p dtb-ke-persist` shows `Compiling
turso_sdk_kit v0.7.2 (…/vendor/turso_sdk_kit-0.7.2)` and `Cargo.lock`'s
entry for it has no `source =` line any more (a local path dependency, not
a registry one) — the Windows-specific `windres` behavior itself can still
only be confirmed on the real Windows runner. MIT-licensed, redistribution
and modification both fine.

## Windows PowerShell encoding gotchas

The Windows jobs' release archive (`DTB Kampfrichtereinsatzpläne-*.zip`) has "ä"
in its name and in the shipped `.exe`'s containing folder name — real, live
releases showed **two separate, distinct encoding bugs** here, both found from
a real user download rather than guessed, and both now fixed:

- **The archive's own name came out as `...KampfrichtereinsatzplÃ¤ne...` on
  Codeberg** — not a display glitch, the asset's real name. Root cause:
  Windows Server only ships **Windows PowerShell 5.1** (`powershell.exe`),
  which — unlike `pwsh` 7 — reads a script file with no UTF-8 BOM using the
  system's ANSI code page rather than assuming UTF-8. Whatever encoding
  Forgejo's runner actually wrote the composited step script in, a literal
  `"ä"` typed into the YAML's `run: |` block came out misdecoded the moment
  PowerShell parsed its own source, before a single byte of our own code (the
  already-fixed `url_encode` in `codeberg.rs`) ever ran — percent-encoding a
  string that's already wrong just transmits the wrong string faithfully.
  Fixed by building the character from its code point instead of typing it
  literally: `$ae = [char]0x00E4` is plain ASCII in the script source, so it
  can't be misread regardless of the file's actual encoding or which
  PowerShell reads it, then `"...pl${ae}ne"` for both `$folder` and `$name`.
- **The `.exe` inside the zip came out as `...Kampfrichtereinsatzpl„ne.exe`
  on extraction** — a *different* garbled character than the archive-name
  bug above, meaning a *second*, independent problem specific to names
  *inside* the zip. Root cause: the archive was built with `tar -a -cf`
  (Windows' built-in `tar.exe`, which is bsdtar) — bsdtar's zip writer does
  not set the UTF-8 language-encoding flag (general-purpose bit 11) on a
  non-ASCII entry name, so any extractor that (correctly, per the classic
  zip spec) falls back to a legacy code page for an unflagged entry decodes
  it wrong — and picks a *different* wrong code page than whatever produced
  the first bug, hence the different mojibake. Fixed by switching to
  PowerShell's built-in `Compress-Archive` cmdlet (.NET's `ZipArchive`),
  which does set that flag correctly for a non-ASCII entry name — no new
  tool dependency, `Compress-Archive` ships with every PowerShell.

Neither fix has been verified against a real Windows run yet (both were
diagnosed and fixed from a user's real download + gpui/bsdtar's documented
behavior, not from a live debugging session on the runner itself) — the next
Tip/Release run is the actual test. `release.yml`'s `zipsign sign zip`
step, which rewrites the same archive in place afterward, should be
unaffected either way (it doesn't touch entry names, only appends signature
data) but hasn't been separately re-verified against a `Compress-Archive`-
built zip specifically.

## Debug info: dSYM / dwp / PDB alongside the release binaries

CI uploads each platform's separate debug-info file — macOS's `.dSYM`,
Linux's `.dwp`, Windows' `.pdb` — so a real crash minidump can be symbolised
offline (`dtb-ke-symbolize`, `minidump-stackwalk`, `lldb`, WinDbg) without
redistributing full debuginfo in the shipped binary. macOS/Linux get theirs
from `[profile.release] split-debuginfo = "packed"` (workspace `Cargo.toml`,
`debug = "line-tables-only"`) exactly as the rustc book describes; Windows
needed a real investigation to get there, since that same setting is a dead
end on `gnullvm` — established by testing rather than by trusting the rustc
book's summary table at face value:

- **macOS / Linux**: `packed` is genuinely, stably supported (per the rustc
  book, confirmed for macOS by an actual local build — `target/release/
  <bin>.dSYM` appears exactly as documented). `cargo dtb-ke-bundle debug-info`
  (new subcommand, `crates/dtb-ke-bundle/src/debug_info.rs`) locates it (`.dSYM` on
  macOS, `.dwp` on Linux — unverified on Linux specifically, no linker for
  that target on the dev machine, but it's the platform the rustc book is
  most confident about) and `tar -czf`s it into its own archive, entirely
  separate from `bundle`'s own packaging so it can never leak into an
  installed `.deb`/`.rpm`/`.AppImage` (those all share `linux::stage_prefix`,
  which `debug_info.rs` never touches). `--universal` merges the two
  `apple-darwin` slices' `.dSYM`s with `lipo` on the inner DWARF binary,
  mirroring how `bundle::build_universal` merges the slices' actual
  executables. Uploaded via `codeberg upload --plain` — see below for why
  `--plain` specifically.
- **Windows (gnullvm)**: `split-debuginfo=packed` does not work at all, full
  stop — tested directly rather than assumed. `-C split-debuginfo=packed -Z
  unstable-options` on a real nightly toolchain, cross-compiling to
  `x86_64-pc-windows-gnullvm`: accepted with **no error**, but the resulting
  `.exe` carries **zero debug sections** (`objdump -h` — nothing, not even
  with `-C debuginfo=2` forcing full debug info) and no sidecar file of any
  kind appears. This matches a real, dated (May 2025) `users.rust-lang.org`
  report of the identical `error: -Csplit-debuginfo=packed is unstable on
  this platform` wall on plain `windows-gnu`, closed with no resolution, and
  the upstream tracking issue (`rust-lang/rust#135531`) titled exactly
  "`-C split-debuginfo=…` is (effectively) untested on windows-msvc and
  windows-gnu" — a real, currently-unfixed gap in the Rust toolchain for
  this target family, not something wrong on our end.

  **A real, separate `.pdb` comes out anyway — a completely different
  mechanism, nothing to do with `split-debuginfo` at all.** rustc's target
  spec for `x86_64-pc-windows-gnullvm` names `x86_64-w64-mingw32-clang` as
  the linker (`linker-flavor = "gnu-cc"` — confirmed via `rustc -Z
  unstable-options --print target-spec-json`), i.e. clang is used purely as a
  *driver*; llvm-mingw's own `<triple>-ld` is a thin wrapper that just execs
  `ld.lld` (confirmed by reading the actual wrapper script, a plain shell
  script shipped in llvm-mingw). `ld.lld`'s GNU-flavor COFF driver genuinely
  supports `--pdb=<file>` (confirmed: `x86_64-w64-mingw32-ld --help` lists it
  verbatim: *"Output PDB debug info file, chosen implicitly if the argument
  is empty"*) — a real capability of LLD's COFF backend, independent of
  whether `-C split-debuginfo` works on this target. Passed a bare `--pdb=`
  through clang, which forwards any flag it doesn't recognise itself straight
  to the linker via the standard `-Wl,<flag>` passthrough mechanism.
  Confirmed working end to end on a real build:
  `RUSTFLAGS="-C link-arg=-Wl,--pdb=./x.pdb -C strip=symbols" cargo build
  --target x86_64-pc-windows-gnullvm --release` produced a file `file`
  identifies as *"MSVC program database ver 7.00"* — a real, valid PDB, not
  a guess — **and** the `.exe` came back down to its normal small stripped
  size with zero debug sections (confirmed via `objdump -h`): `strip=symbols`
  runs as a separate step *after* the linker has already written the PDB, so
  it cleanly strips the binary without touching the file already on disk.
  This is the *same* clean "small stripped binary + separate debug-info
  file" split macOS and Linux get from `packed` — Windows just reaches it by
  a different route, since `packed` itself is a dead end here.

  Wired up via `.cargo/config.toml`'s `[target.x86_64-pc-windows-gnullvm]`/
  `[target.aarch64-pc-windows-gnullvm]` `rustflags`, one fixed
  `-Wl,--pdb=<literal filename>.pdb` each (`dtb-ke-ui-x86_64.pdb` /
  `dtb-ke-ui-aarch64.pdb` — distinct names so a local dev machine building
  both sequentially can't clobber one with the other). The path can't be
  `$CARGO_TARGET_DIR`-aware — rustflags in a config file can't be templated,
  and CI always overrides that env var to an absolute path elsewhere anyway
  (see "Target-dir caching" above) — so it's a plain relative filename
  instead, which resolves against whatever the *linker's* actual working
  directory is. Confirmed that's the **workspace root**: every cargo
  invocation this project makes goes through `dtb-ke-bundle`'s own
  `util::run`, which always sets `current_dir(workspace_root())` — a real
  test with a relative `--pdb=./x.pdb` landed exactly there, not inside
  `target/`. `util::debug_info_path` (`dtb-ke-bundle`) knows this fixed
  location for these two triples; `.gitignore`'s `*.pdb` covers the loose
  file this leaves at the repo root for a local build. `cargo dtb-ke-bundle
  debug-info --target <triple>` picks it up as a plain file (no special
  casing needed vs. Linux's `.dwp` — `tar_entry` archives a file or a
  directory tree alike) and `codeberg upload --plain` ships it, same as the
  other two platforms.

  *(Switching the Windows target from `gnullvm` to `msvc` was considered —
  there, `packed` → a real `.pdb` is one of the officially stable
  combinations, no linker-flag archaeology needed at all. Given the fix
  above already gets a real PDB without switching, and this project's
  Windows target is `gnullvm` by an explicit prior user requirement unrelated
  to debug info — switching would still mean re-verifying the crash-handler's
  Windows FFI (`dtb-ke-crash`) under a different C ABI/unwind-info
  convention, a real risk to already-working, safety-critical infrastructure
  — there's no reason left to reopen that question over this.)*

  **What this PDB actually contains — verified against a real CI-built binary, not assumed.**
  Downloaded a real `.exe` + `.pdb` pair from an actual Tip run and checked them with
  `llvm-readobj`/`llvm-pdbutil`/`llvm-symbolizer` (all shipped by llvm-mingw): the `.exe`'s embedded
  CodeView debug directory (`llvm-readobj --coff-debug-directory`) names the exact PDB GUID/age, which
  matches the PDB's own header (`llvm-pdbutil dump --summary`) exactly — a genuinely matched pair, not
  just two files that happen to sit together. The PDB carries **354,797 real public symbols** —
  genuine demangled project code (`dtb_ke_ui::components::menu_bar::MenuBar::render_strip`,
  `dtb_ke_ui::store::AppStore::import_decoded`, …) — and `llvm-symbolizer --obj=<exe> <address>`
  correctly resolves arbitrary code addresses to the right function name every time.

  **But it has no file/line info at all** — `llvm-pdbutil dump -l` shows zero line records in every
  `dtb_ke_ui` module, and every `llvm-symbolizer` lookup comes back `??:0:0` for the source location,
  regardless of `debug = "line-tables-only"` vs. full `debug = 2` (tested both directly — identical
  result). This turned out to be a genuine, confirmed **upstream limitation of LLD's COFF PDB writer
  itself**, not a missing flag or a tuning problem: read LLD's actual `lld/COFF/PDB.cpp` from upstream
  and confirmed it contains **zero** occurrences of "dwarf", "debug_line", "debug_info", or `.debug_`
  anywhere in the file — it only ever reads native CodeView `.debug$S` subsections
  (`DebugSHandler::handleDebugS`, dispatching on `DebugSubsectionKind::Lines`/`InlineeLines`) for
  line-table data, which our pipeline never produces (rustc's own LLVM codegen emits DWARF for
  `windows-gnu`/`gnullvm` targets unconditionally — there's no rustc `-C`/`-Z` flag to ask for CodeView
  instead, and `-gcodeview` is a *clang frontend* flag that would need clang to actually compile the
  code, which it never does here — clang only runs as the linker driver). The 354K public symbols come
  from a completely separate function, `PDBLinker::addPublicsToPDB()`, which walks the linker's own
  symbol table (`ctx.symtab.forEachSymbol`) independent of whatever's in `.debug$S` — confirmed this
  runs, and produces Publics, even with zero CodeView debug subsections present. Also checked
  `lld/COFF/MinGW.cpp` (LLD's MinGW-mode driver logic specifically) for any DWARF→CodeView conversion
  pass that might feed `PDB.cpp` — none exists; `--pdb=` reaches the same generic, CodeView-only writer
  regardless of MinGW mode. **Net effect**: function-name-level symbolication works and is genuinely
  useful; file/line does not and structurally can't, without either switching to MSVC (real CodeView
  from the ground up) or converting DWARF to Breakpad `.sym` with a dedicated tool like Mozilla's
  `dump_syms` (built for exactly this MinGW+DWARF scenario) instead of using the PDB path at all —
  neither pursued, both left as a known, deliberate limitation of the current setup.

**`codeberg upload --plain`** (new flag, `dtb-ke-bundle`): every upload
normally also generates a small `<name>.fragment.json` sidecar that
`publish_manifest` later folds into the self-update `manifest.json` — correct
for real app archives, wrong for a debug-info archive, which must never look
like a downloadable app update to `self_update`. `--plain` skips the
fragment entirely (`Client::upload_plain`, vs. the fragment-generating
`Client::upload`).

## iOS job (`ios` in `tip.yml` / `release.yml`)

Also uploads the app's dSYM (`…-aarch64-apple-ios.debuginfo.tar.gz`, `--plain`): iOS crash reports carry only image UUIDs + offsets, and the dSYM is what symbolicates them (see CLAUDE.md, Crash capture → iOS).

Runs on `macos-host` and calls `cargo dtb-ke-bundle bundle --release --target aarch64-apple-ios --mac-wrapper`,
producing `DTB Kampfrichtereinsatzpläne.app`, `.ipa` (zip of `Payload/<app>`), and
`mac-wrapper/<app>.app` (the "Designed for iPad" wrapper macOS launches iOS apps from, not a real
Catalyst build). Uploaded with `--plain` as `…-aarch64-apple-ios{.ipa,.app.tar.gz,-mac-wrapper.tar.gz}`
so the in-app updater's manifest never lists them. `publish` deliberately does **not** `need` this job —
an iOS failure must not block the desktop release.

- **Signing**: `IOS_SIGN_IDENTITY` + `IOS_PROVISIONING_PROFILE` (see Secrets); otherwise ad-hoc.
- **SDK**: like the macOS leg, `dtb-ke-bundle` restamps the binary's `LC_BUILD_VERSION` SDK up to
  `meta::IOS_SDK_FLOOR` (26.0) with `vtool`, because Liquid Glass is gated on the *linked* SDK.
- **Toolchain on the legacy Mac**: iOS needs a **full Xcode** (Command Line Tools have no iPhoneOS SDK) —
  the newest one macOS 13 runs is Xcode 15.2. The app icon is *not* compiled on the runner: `Assets.car`, the fallback PNGs and the icon
  keys are committed under `assets/icons/generated/ios/` (`cargo dtb-ke-bundle icons` on a Mac with Xcode 26).
  Remaining risk: the Metal 4 shader path in `gpui_apple` may not compile against the old Metal toolchain. `runner-setup-macos.sh` installs the
  `aarch64-apple-ios` Rust target; installing Xcode is manual. Not yet run for real.

## Secrets

Set these under Codeberg → this repo → **Settings → Actions → Secrets**:

| secret | used by | what it is |
|---|---|---|
| `CODEBERG_TOKEN` | both workflows, every job that touches the API | a Codeberg personal access token with `repo` (write) scope — creates/updates releases, uploads assets, force-pushes the rolling `tip`/`latest` tags |
| `ZIPSIGN_KEY` | both workflows (every archive on both channels is zipsign-signed) | base64 of the zipsign **private** key (`base64 -w0 release.priv`) — see `UPDATER.md` § 1 for key generation |
| `IOS_SIGN_IDENTITY` + `IOS_PROVISIONING_PROFILE` (optional, both needed) | `tip.yml` / `release.yml`'s `ios` job | the `Apple Development: …` / `Apple Distribution: …` identity present in the macOS runner's keychain, and the base64 of a `.mobileprovision` for `de.philippremy.DTB-Kampfrichtereinsatzplaene`. Unset → ad-hoc signing (artifacts build but an iPad won't install them). The Mac wrapper only launches on a Mac if the profile allows Apple Silicon Macs |
| `MACOS_SIGN_IDENTITY` (optional) | `release.yml`'s `macos-universal` job | the `Developer ID Application: …` identity string; unset falls back to ad-hoc signing and skips notarization (see above) rather than failing the job |
| `APPLE_API_KEY_ID`, `APPLE_API_ISSUER_ID`, `APPLE_API_KEY_P8` (optional, only matter with `MACOS_SIGN_IDENTITY` set) | same job, notarization | an App Store Connect API key (developer.apple.com → Users and Access → Integrations → Keys); `APPLE_API_KEY_P8` is the base64 of the downloaded `.p8` file |
| `DTB_KE_SYMBOLS_URL`, `DTB_KE_SYMBOLS_UPLOAD_TOKEN` | the Windows, macOS and iOS jobs' "Upload debug symbols" step (both workflows) | the symbol server (`dtb-ke-symbol-server`, on the home Pi) — URL like `https://symbols.<domain>` and the **upload** token from the Pi's `/etc/dtb-ke-symbol-server.env`. `cargo dtb-ke-bundle symbols upload` PUTs each build's dSYM / PDB keyed by debug id. Unset = skipped with a note on Tip; `release.yml` passes `--strict`, so a release fails without them. The URL is a secret only to keep the home domain out of a public repo. **The runners must resolve that name to the Pi from inside the LAN** (router NAT loopback, or a hosts / local-DNS override to the Pi's LAN address). Linux keeps its Codeberg `.debuginfo.tar.gz`: a `.dwp` carries no debug id, so there is nothing to key it by on the server |
| `DTB_KE_SMTP_HOST`, `DTB_KE_SMTP_PORT`, `DTB_KE_SMTP_USER`, `DTB_KE_SMTP_PASS`, `DTB_KE_SMTP_FROM`, `DTB_KE_SMTP_TO` (optional) | both workflows, every build job (top-level `env:`) | the crash-reporter/feedback-window mail transport's credentials, baked in at compile time by `dtb-ke-ui/build.rs::emit_smtp_secret` (see `mail.rs`) — unset leaves the feature compiled in but disabled (`mail::available()` false), never a build failure |

`CODEBERG_TOKEN` is also what `scripts/release.sh` needs *not* have —
deliberately: the script only ever does local git operations (bump, tag,
push); every actual Codeberg API call happens in CI once the tag lands,
using the secret above. Nothing on a developer's own machine ever needs
`CODEBERG_TOKEN`.

## Cutting a release

`scripts/release.sh [x.y.z]` — bumps the workspace version, opens `$EDITOR`
on a release-notes template pre-filled with the commit list and
`git shortlog` contributors since the last version tag, creates an
**annotated** `vX.Y.Z` tag with those notes as the tag message (which
`release.yml`'s `prepare` job reads back with
`git tag -l --format='%(contents)'`, then `sed`s off the trailing PGP block
if the tag is signed — `git config tag.gpgsign` — rather than re-deriving
anything), atomic-pushes `main` + the tag, then bumps `main` again to
`x.y.(z+1)-dev.0` (see UPDATER.md § 3) so `tip` sits one commit ahead of
`latest` and semver-greater. See the script's own comments for the exact
sequence; see `release.yml` for what happens after the tag lands.

`scripts/release.sh --force x.y.z` re-cuts an **existing** version: same flow,
but it seeds the editor with the tag's current notes, `git tag -f`, and
force-pushes only the tag (`+refs/tags/vX.Y.Z`). Use it to re-trigger
`release.yml` after a CI-config fix; the extra `chore:` commits stay in
history until rebased out. A plain (non-`--force`) run refuses if the tag
exists.
