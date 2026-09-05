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
/// `filesystem::APPLICATION_IDENTIFIER` and `build_info::IDENTIFIER`.
pub const IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzpläne";

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

/// Minimum macOS **SDK** version stamped into the built binary's
/// `LC_BUILD_VERSION` (`macos::ensure_min_sdk`). macOS 26 ("Tahoe") gates its
/// redesigned interface — larger window controls, Liquid Glass chrome — on the
/// SDK the binary was *linked against* being ≥ this, independent of the OS it
/// runs on. A build on an older macOS (the Intel CI host tops out at the 13.x
/// SDK) would otherwise ship the pre-redesign look even on Tahoe. Only the
/// recorded SDK is touched; the deployment target ([`MACOS_MIN_VERSION`]) is
/// left alone. Raise this when a later redesign moves the gate. See RUNNERS.md.
pub const MACOS_SDK_FLOOR: &str = "26.0";

/// **Stable** WiX upgrade code — a fixed GUID that identifies the product line
/// across versions so an `.msi` upgrades in place. Never change this.
pub const WIX_UPGRADE_CODE: &str = "8F3A1C57-2D94-4E6B-9A11-6C0F5B2E7D84";

/// `CFBundleVersion` / MSI `ProductVersion` want `x.y.z[.b]`; our SemVer string
/// is already in that shape, but strip any pre-release / build metadata.
pub fn numeric_version() -> &'static str {
    VERSION.split(['-', '+']).next().unwrap_or(VERSION)
}
