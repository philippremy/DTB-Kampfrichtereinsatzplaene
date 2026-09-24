//! Everything the packagers need to know about the product, in one place.
//!
//! Keep these in sync with `crates/dtb-ke-ui` (`filesystem.rs`'s identifier,
//! `build_info.rs`'s copyright, the `[[bin]]` name) and the workspace version.

/// The raw cargo build artifact's name — must match `crates/dtb-ke-ui/
/// Cargo.toml`'s `[[bin]] name` *exactly*, since this is only ever used to
/// locate what cargo actually built (`util::built_binary_path` and friends).
/// Deliberately kebab-case, unlike the shipped product name below — see that
/// Cargo.toml's own comment for why.
pub const RAW_BIN_NAME: &str = "dtb-ke-ui";

/// The file dsymutil generates inside the .dsym bundle, it is simply
/// RAW_BINARY_NAME converted to snake case.
pub const RAW_DSYM_NAME: &str = "dtb_ke_ui";

/// The name shown to users — window titles, the `.app`, Start-menu entries,
/// and (applied by each packager at package time, not by cargo at build
/// time) the actual shipped executable's file name inside the portable
/// Windows folder. **Not** the macOS executable file name — see
/// [`MACOS_EXECUTABLE_NAME`].
pub const DISPLAY_NAME: &str = "DTB Kampfrichtereinsatzpläne";

/// The file name of the executable inside the macOS `.app`
/// (`Contents/MacOS/`), and its `CFBundleExecutable`. An ASCII
/// transliteration of [`DISPLAY_NAME`] rather than the name itself: `codesign`
/// on macOS 26 cannot ad-hoc-sign a bundle whose main executable's file name
/// carries a non-ASCII character, and even a forced signature fails
/// `codesign --verify --strict` (see `macos.rs` / RUNNERS.md). The bundle
/// *directory*, `CFBundleName` and `CFBundleDisplayName` all keep
/// `DISPLAY_NAME`, so this only surfaces in Activity Monitor / `ps`.
pub const MACOS_EXECUTABLE_NAME: &str = "DTB-Kampfrichtereinsatzplaene";

/// Reverse-DNS bundle / application identifier. Must match
/// `filesystem::APPLICATION_IDENTIFIER` and `build_info::IDENTIFIER`. Carries
/// the non-ASCII `ä` because it's the data-directory / preferences-domain key
/// on macOS and changing it would strand existing installs' databases — so it
/// is **not** used for the freedesktop artifacts below (see [`RDNS_ID`]).
pub const IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzpläne";

/// ASCII reverse-DNS app ID for every freedesktop artifact on Linux: the
/// `.desktop` file, the AppStream metainfo file *and* its `<id>`, the hicolor
/// icon file names. An AppStream component ID must be `[A-Za-z0-9._-]` only —
/// `appstreamcli validate` (which `appimagetool` runs and treats as fatal)
/// rejects the `ä` in [`IDENTIFIER`] with `cid-invalid-character`. Uses the
/// same transliteration as [`MACOS_EXECUTABLE_NAME`].
pub const RDNS_ID: &str = "de.philippremy.DTB-Kampfrichtereinsatzplaene";

/// An ASCII-only slug for paths that dislike spaces / non-ASCII: the Linux
/// binary name, the tarball stem, the AppDir, the RPM/DEB package name.
pub const SLUG: &str = "dtb-ke-kampfrichtereinsatzplaene";

/// Workspace version (`[workspace.package] version`, inherited by this crate).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const PUBLISHER: &str = "Philipp Remy";
pub const COPYRIGHT: &str = "© 2026 Philipp Remy. Freie Software, lizenziert unter der GNU Affero General Public License v3.";
pub const HOMEPAGE: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene";

/// One-line description (German — user-facing).
pub const SUMMARY: &str =
    "Kampfrichtereinsatzpläne für Rhönrad- und Cyr-Wheel-Wettkämpfe des DTB erstellen";

/// A short paragraph for package metadata (German).
pub const DESCRIPTION: &str = "\
DTB Kampfrichtereinsatzpläne ist eine native Anwendung zum Erstellen von \
Kampfrichtereinsatzplänen für Rhönrad- und Cyr-Wheel-Wettkämpfe im Deutschen \
Turner-Bund. Kampfgerichte werden mit Namen besetzt, in Qualifikation und Finale \
gegliedert und als PDF oder Word-Dokument exportiert.";

/// SPDX identifier (`[workspace.package] license`).
pub const LICENSE: &str = "AGPL-3.0-or-later";

/// freedesktop menu categories for the `.desktop` file.
pub const FREEDESKTOP_CATEGORIES: &str = "Office;";

/// `LSApplicationCategoryType` for the macOS `Info.plist`.
pub const MACOS_CATEGORY: &str = "public.app-category.productivity";

/// Lowest macOS the app is expected to run on.
pub const MACOS_MIN_VERSION: &str = "11.0";

/// Lowest iOS / iPadOS the app is expected to run on (matches the `gpui_ios` backend's floor;
/// `bundle` also exports it as `IPHONEOS_DEPLOYMENT_TARGET` for the build).
pub const IOS_MIN_VERSION: &str = "15.0";

/// Minimum macOS **SDK** version stamped into the built binary's
/// `LC_BUILD_VERSION` (`macos::ensure_min_sdk`). macOS 26 ("Tahoe") gates its
/// redesigned interface — larger window controls, Liquid Glass chrome — on the
/// SDK the binary was *linked against* being ≥ this, independent of the OS it
/// runs on. A build on an older macOS (the Intel CI host tops out at the 13.x
/// SDK) would otherwise ship the pre-redesign look even on Tahoe. Only the
/// recorded SDK is touched; the deployment target ([`MACOS_MIN_VERSION`]) is
/// left alone. Raise this when a later redesign moves the gate. See RUNNERS.md.
pub const MACOS_SDK_FLOOR: &str = "26.0";

/// The `.dtbke` file type — a single-competition postcard blob (see
/// `dtb-ke-ui/src/save/mod.rs` / `store.rs::export_competition`; **not** the
/// whole-database backup, which is a plain `.bin` file with no registered
/// type — see that crate's `filesystem.rs`). Registered on every platform so
/// double-clicking one imports it via `dtb-ke-ui/src/open_files.rs` +
/// `app::open_paths`.
pub const DOC_EXTENSION: &str = "dtbke";

/// The user-facing name for the `.dtbke` file type — a macOS `ProgId`/
/// `UTType` description, a Windows `ProgId` description, and the Linux
/// shared-mime-info `<comment>`.
pub const DOC_TYPE_NAME: &str = "DTB Kampfrichtereinsatzplan";

/// The freedesktop / Windows MIME type for `.dtbke` — there's no registered
/// vendor type for this, so this follows the conventional unofficial
/// `application/x-<ext>` shape (e.g. Blender's `.blend` as
/// `application/x-blender`).
pub const DOC_MIME_TYPE: &str = "application/x-dtbke";

/// The macOS Uniform Type Identifier exported for `.dtbke`
/// (`UTExportedTypeDeclarations` in `macos.rs`'s Info.plist). Reverse-DNS
/// under the same domain as [`IDENTIFIER`], but ASCII-only like [`RDNS_ID`] —
/// UTIs are conventionally ASCII and there's no existing install to strand
/// by keeping it that way from the start. A UTI cannot contain "Umlaute",
/// so we turn "Kampfrichtereinsatzpläne" to "Kampfrichtereinsatzplaene".
pub const DOC_UTI: &str = "de.philippremy.DTB-Kampfrichtereinsatzplaene.DTB-Kampfrichtereinsatzplan";

/// **Stable** WiX upgrade code — a fixed GUID that identifies the product line
/// across versions so an `.msi` upgrades in place. Never change this.
pub const WIX_UPGRADE_CODE: &str = "8F3A1C57-2D94-4E6B-9A11-6C0F5B2E7D84";

/// `CFBundleAlternateNames` — short nicknames Spotlight/Siri also match
/// against, alongside [`DISPLAY_NAME`] itself. Useful here specifically
/// because the real name is long and carries a non-ASCII `ä` that not every
/// input method / keyboard layout types easily.
pub const MACOS_ALTERNATE_NAMES: &[&str] =
    &["DTB KE", "DTB-KE", "Kampfrichtereinsatzpläne", "Kampfrichtereinsatzplaene"];

/// `CFBundleVersion` / MSI `ProductVersion` want `x.y.z[.b]`; our SemVer string
/// is already in that shape, but strip any pre-release / build metadata.
pub fn numeric_version() -> &'static str {
    VERSION.split(['-', '+']).next().unwrap_or(VERSION)
}
