# Shared assets

Workspace-level assets consumed by more than one crate.

## `icons/` — the app icon master

One master, two consumers:

| file                  | notes                                                               |
|-----------------------|---------------------------------------------------------------------|
| `icons/AppIcon.svg`   | preferred — square viewBox, no external fonts/images                |
| `icons/AppIcon.png`   | fallback — square, **≥ 1024×1024**, straight (un-premultiplied) alpha |

`AppIcon.svg` wins if both are present.

- **`dtb-ke-ui`** `include_bytes!`s `icons/AppIcon.png` and shows it in the About
  window (`about.rs`).
- **`dtb-ke-bundle`** derives every platform icon from the master
  (`cargo dtb-ke-bundle icons`, also run by `bundle`), cached in
  `target/bundle/icon/` (git-ignored):
  - `AppIcon.icns` — macOS `.app` (built with `iconutil`; macOS host only)
  - `AppIcon.ico` — Windows `.msi` shortcut / Add-Remove-Programs icon
  - `hicolor/<size>/apps/de.philippremy.DTB-Kampfrichtereinsatzpläne.png` + `scalable/…svg`
    — Linux `.deb` / `.rpm` / `.AppImage` / tarball
  - `icon-512.png` — generic (AppImage `.DirIcon`)
