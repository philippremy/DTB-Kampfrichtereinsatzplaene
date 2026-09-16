# Shared assets

Workspace-level assets consumed by more than one crate.

## `icons/` — the app icon

`icons/AppIcon.icon` is the **master** — an Xcode 26 Icon Composer package
(`icon.json` + its source images/materials under `Assets/`), edited with the
Icon Composer app. It is **not** a flat image: it describes the Liquid Glass
icon's layers, materials and translucency, and can only be compiled by Apple's
own `actool`/`iconutil` (Xcode 26+, macOS only — there's no portable renderer
for it).

Everything else is **derived** by `cargo dtb-ke-bundle icons` and committed
as regular, git-tracked files under `icons/generated/` (there is no way to
regenerate them on Windows/Linux, so — unlike the old flat SVG/PNG master —
they can't just be rebuilt from source on every host at bundle time):

| file                                                | notes                                                                 |
|------------------------------------------------------|------------------------------------------------------------------------|
| `generated/AppIcon.icns`                              | pre-Tahoe fallback — a fully rasterised, multi-size standalone icon, flattened out of the Liquid Glass layers (`--standalone-icon-behavior all`) |
| `generated/Assets.car`                                | the compiled asset catalog carrying the *actual* Liquid Glass icon; embedded at `Contents/Resources/Assets.car`, looked up by `CFBundleIconName` |
| `generated/AppIcon.png`                               | a flat 1024×1024 PNG, extracted from the fallback `.icns`'s largest rendition — the source every OS-agnostic derivation below works from |
| `generated/AppIcon.ico`                               | Windows `.msi` shortcut / Add-Remove-Programs icon                     |
| `generated/hicolor/<size>/apps/de.philippremy.DTB-Kampfrichtereinsatzpläne.png` | Linux `.deb` / `.rpm` / `.AppImage` / tarball                          |
| `generated/icon-512.png`                              | generic (AppImage `.DirIcon`)                                          |

- **`dtb-ke-ui`** `include_bytes!`s `icons/generated/AppIcon.png` and shows it
  in the About window (`about.rs`).
- **`dtb-ke-bundle`** just consumes `icons/generated/` as-is on every host
  (`bundle.rs`'s `icon::available()`); it never tries to regenerate it.

**Whenever `AppIcon.icon` changes**, run `cargo dtb-ke-bundle icons` on a Mac
with Xcode 26+ installed and commit the regenerated `icons/generated/` — see
`crates/dtb-ke-bundle/src/icon.rs`'s module doc for the full pipeline
(`actool --compile` → icns + Assets.car, `iconutil --convert iconset` → the
flat PNG, then pure-Rust resizing for the `.ico`/hicolor sets).
