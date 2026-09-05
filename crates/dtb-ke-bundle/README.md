# dtb-ke-bundle

Build orchestration + platform packaging for **DTB Kampfrichtereinsatzpläne**.
Invoked through the workspace alias:

```bash
cargo dtb-ke-bundle <command>
```

| command                              | what it does                                                         |
|--------------------------------------|---------------------------------------------------------------------|
| `build [--release]`                  | build + ad-hoc-sign the crash helper, stage it, then build the app   |
| `helper [--release]`                 | just (re)stage the crash helper                                      |
| `icons`                              | regenerate every platform icon from `assets/icons/AppIcon.{svg,png}` |
| `bundle [--debug] [--formats …] [--sign <id>]` | build + package for the **host** OS                     |

`build` / `helper` are the old `xtask` behaviour and are unchanged — `dtb-ke-crash`
`include_bytes!`s the staged helper, so it must be built before the app or crash
capture is disabled (the panic hook still installs).

## Icons

The master lives at the workspace root, [`assets/icons/`](../../assets/README.md),
shared with `dtb-ke-ui` (which embeds `AppIcon.png` for its About window):

- `AppIcon.svg` — preferred (square viewBox)
- `AppIcon.png` — fallback (square, ≥ 1024×1024, straight alpha)

Everything else is derived and cached in `target/bundle/icon/` (git-ignored):
`.icns` (macOS `iconutil`), a multi-resolution `.ico`, and a freedesktop
`hicolor/` PNG set + the scalable SVG.

## Bundling

`bundle` builds the app for the host and packages it. Because the workspace
can't cross-compile `dtb-ke-ui`, each platform's packages are only produced on
that platform.

### macOS

Produces `target/bundle/<profile>/DTB Kampfrichtereinsatzpläne.app` — a
self-contained bundle (the binary embeds its assets, fonts and the crash
helper). Signed **ad-hoc** by default; for distribution:

```bash
cargo dtb-ke-bundle bundle --sign "Developer ID Application: … (TEAMID)"
# or: DTB_KE_SIGN_ID="Developer ID Application: …" cargo dtb-ke-bundle bundle
```

`--formats dmg` additionally wraps it in a compressed `.dmg` (`hdiutil`).
Notarisation (`xcrun notarytool` / `stapler`) is not automated yet.

### Windows

Produces a portable `<slug>-<ver>-x64/` folder and a WiX authoring file
`wix/Package.wxs`. If the **WiX Toolset v4** is on `PATH`
(`dotnet tool install --global wix`) it runs `wix build` to produce the
`.msi`; otherwise it stops at the `.wxs`, which can be built on another
machine.

### Linux

From one `/usr`-prefixed payload tree:

| format      | how                              | requires            |
|-------------|----------------------------------|---------------------|
| `.tar.gz`   | relocatable, with `install.sh`   | `tar` (always)      |
| `.deb`      | assembled directly (`ar` + `tar`)| `tar`, `md5sum`     |
| `.rpm`      | `.spec` → `rpmbuild -bb`          | `rpmbuild`          |
| `.AppImage` | `AppDir` → `appimagetool`        | `appimagetool`      |

`--formats deb,tar` restricts the set. Missing `rpmbuild` / `appimagetool`
leaves the intermediate tree with a note instead of failing.

The `.deb` `Depends:` line is a conservative hand-maintained list in
`src/linux.rs` (`DEB_DEPENDS`) — check it against `ldd target/release/<bin>`
when the runtime library set changes.
