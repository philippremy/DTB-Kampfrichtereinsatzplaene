//! Everything the packagers need to know about the product, in one place.
//!
//! Keep these in sync with `crates/dtb-ke-ui` (`filesystem.rs`'s identifier,
//! `build_info.rs`'s copyright, the `[[bin]]` name) and the workspace version.

/// The iPadOS home-screen label (`CFBundleDisplayName`). The full name is cut off after about 13
/// characters under the icon ("DTBKampfrichte…"), so iOS gets an abbreviation: KE for
/// KampfrichterEinsatzpläne, which is how the sport shortens it.
pub const IOS_DISPLAY_NAME: &str = "DTB KE-Pläne";

/// Everything that differs between the shipped products. The bundler builds one product per
/// invocation (`--product app|debugger`, default `app`); every packager reads it through [`p()`].
pub struct Product {
    /// The cargo package to build (`cargo build -p …`).
    pub package: &'static str,
    /// The raw cargo build artifact's name — must match the package's `[[bin]] name` *exactly*; only used to
    /// locate what cargo actually built. Deliberately kebab-case, unlike the shipped product name.
    pub raw_bin_name: &'static str,
    /// The file dsymutil generates inside the .dSYM bundle (`raw_bin_name` in snake case).
    pub raw_dsym_name: &'static str,
    /// The name shown to users — window titles, the `.app`, Start-menu entries, the portable Windows folder's
    /// executable. **Not** the macOS executable file name — see `macos_executable_name`.
    pub display_name: &'static str,
    /// The file name of the executable inside the macOS `.app` (`Contents/MacOS/`), and its
    /// `CFBundleExecutable`. ASCII: `codesign` on macOS 26 cannot ad-hoc-sign a bundle whose main executable's
    /// file name carries a non-ASCII character (see `macos.rs` / RUNNERS.md).
    pub macos_executable_name: &'static str,
    /// Reverse-DNS bundle / application identifier.
    pub identifier: &'static str,
    /// ASCII reverse-DNS app ID for every freedesktop artifact (`.desktop`, AppStream `<id>`, hicolor icon
    /// names) — `appstreamcli validate` rejects non-ASCII component ids.
    pub rdns_id: &'static str,
    /// ASCII name for Linux paths: the binary name, tarball stem, AppDir, RPM/DEB package name.
    pub slug: &'static str,
    /// One-line description (German — user-facing).
    pub summary: &'static str,
    /// A short paragraph for package metadata (German).
    pub description: &'static str,
    /// freedesktop menu categories for the `.desktop` file.
    pub freedesktop_categories: &'static str,
    /// `LSApplicationCategoryType` for the macOS `Info.plist`.
    pub macos_category: &'static str,
    /// `CFBundleAlternateNames` — short nicknames Spotlight/Siri also match against.
    pub macos_alternate_names: &'static [&'static str],
    /// **Stable** WiX upgrade code — a fixed GUID identifying the product line across versions. Never change.
    pub wix_upgrade_code: &'static str,
    /// The file type this product registers: extension (no dot).
    pub doc_extension: &'static str,
    /// User-facing name of that type (macOS `UTType` description, Windows `ProgId`, shared-mime-info `<comment>`).
    pub doc_type_name: &'static str,
    /// The freedesktop / Windows MIME type (`application/x-<ext>`; there is no registered vendor type).
    pub doc_mime_type: &'static str,
    /// The macOS UTI exported for the type. ASCII — a UTI cannot contain umlauts.
    pub doc_uti: &'static str,
    /// The Windows `ProgId` of the file type — a plain short id (the registry does not care, and it reads well in `regedit`).
    pub doc_progid: &'static str,
    /// The role this product plays for that type in Launch Services (`Editor` / `Viewer`).
    pub doc_role: &'static str,
    /// Workspace-relative folder for this product's committed `generated/` icons.
    pub icon_dir: &'static str,
    /// Workspace-relative flat PNG (non-square is fine) used as the document-type icon; optional.
    pub doc_icon_master: &'static str,
    /// Workspace-relative Icon Composer master (`*.icon`). `actool` compiles it under the fixed asset name
    /// `AppIcon` for every product (see `icon::generate_app_icon`).
    pub icon_master: &'static str,
    /// Whether the product embeds the out-of-process crash helper (staged before the build).
    pub crash_helper: bool,
    /// Whether an iPadOS bundle exists for it.
    pub ios: bool,
    /// The AGPL text shipped in the bundle, workspace-relative.
    pub license_file: &'static str,
}

pub static APP: Product = Product {
    package: "dtb-ke-ui",
    raw_bin_name: "dtb-ke-ui",
    raw_dsym_name: "dtb_ke_ui",
    display_name: "DTB Kampfrichtereinsatzpläne",
    macos_executable_name: "DTB-Kampfrichtereinsatzplaene",
    // Carries the non-ASCII `ä` because it is the data-directory / preferences-domain key on macOS (must match
    // `filesystem::APPLICATION_IDENTIFIER`); changing it would strand existing installs' databases.
    identifier: "de.philippremy.DTB-Kampfrichtereinsatzpläne",
    rdns_id: "de.philippremy.DTB-Kampfrichtereinsatzplaene",
    slug: "dtb-ke-kampfrichtereinsatzplaene",
    summary: "Kampfrichtereinsatzpläne für Rhönrad- und Cyr-Wheel-Wettkämpfe des DTB erstellen",
    description: "\
DTB Kampfrichtereinsatzpläne ist eine native Anwendung zum Erstellen von \
Kampfrichtereinsatzplänen für Rhönrad- und Cyr-Wheel-Wettkämpfe im Deutschen \
Turner-Bund. Kampfgerichte werden mit Namen besetzt, in Qualifikation und Finale \
gegliedert und als PDF oder Word-Dokument exportiert.",
    freedesktop_categories: "Office;",
    macos_category: "public.app-category.productivity",
    macos_alternate_names: &[
        "DTB KE",
        "DTB-KE",
        "Kampfrichtereinsatzpläne",
        "Kampfrichtereinsatzplaene",
    ],
    wix_upgrade_code: "8F3A1C57-2D94-4E6B-9A11-6C0F5B2E7D84",
    // A single-competition postcard blob (see `dtb-ke-ui/src/save/mod.rs`); **not** the whole-database backup.
    doc_extension: "dtbke",
    doc_type_name: "DTB Kampfrichtereinsatzplan",
    doc_mime_type: "application/x-dtbke",
    doc_uti: "de.philippremy.DTB-Kampfrichtereinsatzplaene.DTB-Kampfrichtereinsatzplan",
    doc_progid: "DTBKE.Document",
    doc_role: "Editor",
    icon_dir: "assets/icons",
    icon_master: "assets/icons/AppIcon.icon",
    doc_icon_master: "assets/icons/FileIcon.png",
    crash_helper: true,
    ios: true,
    license_file: "crates/dtb-ke-ui/assets/AGPL-3.0.txt",
};

pub static DEBUGGER: Product = Product {
    package: "dtb-ke-debugger",
    raw_bin_name: "dtb-ke-debugger",
    raw_dsym_name: "dtb_ke_debugger",
    display_name: "DTB Kampfrichtereinsatzpläne Debugger",
    macos_executable_name: "DTB-Kampfrichtereinsatzplaene-Debugger",
    identifier: "de.philippremy.DTB-Kampfrichtereinsatzplaene.Debugger",
    rdns_id: "de.philippremy.DTB-Kampfrichtereinsatzplaene.Debugger",
    slug: "dtb-ke-kampfrichtereinsatzplaene-debugger",
    summary: "Absturzberichte (.dtbkedmp) der DTB Kampfrichtereinsatzpläne untersuchen und symbolisieren",
    description: "\
Der Debugger der DTB Kampfrichtereinsatzpläne öffnet die Absturzberichte der \
Hauptanwendung, sucht die passenden Debug-Dateien für genau diesen Build und \
zeigt Stapelspuren, Register und den zugehörigen Quelltext an. \
Ein reines Entwicklerwerkzeug.",
    freedesktop_categories: "Development;Debugger;",
    macos_category: "public.app-category.developer-tools",
    macos_alternate_names: &[
        "DTB KE Debugger",
        "DTB-KE-Debugger",
        "Kampfrichtereinsatzpläne Debugger",
    ],
    wix_upgrade_code: "B7D2E4A9-5C18-4F03-8E6A-1A9D3C70F5B2",
    doc_extension: "dtbkedmp",
    doc_type_name: "DTB Kampfrichtereinsatzpläne Absturzbericht",
    doc_mime_type: "application/x-dtbkedmp",
    doc_uti: "de.philippremy.DTB-Kampfrichtereinsatzplaene.Absturzbericht",
    doc_progid: "DTBKEDebugger.CrashReport",
    doc_role: "Viewer",
    icon_dir: "assets/icons/debugger",
    icon_master: "assets/icons/DebuggerIcon.icon",
    doc_icon_master: "assets/icons/CrashDumpIcon.png",
    crash_helper: false,
    ios: false,
    license_file: "crates/dtb-ke-ui/assets/AGPL-3.0.txt",
};

static SELECTED: std::sync::OnceLock<&'static Product> = std::sync::OnceLock::new();

/// Select the product to package (once, from `--product`).
pub fn select(product: &'static Product) {
    let _ = SELECTED.set(product);
}

/// The selected product ([`APP`] unless `--product` said otherwise).
pub fn p() -> &'static Product {
    SELECTED.get().copied().unwrap_or(&APP)
}

/// Workspace version (`[workspace.package] version`, inherited by this crate).
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub const PUBLISHER: &str = "Philipp Remy";
pub const COPYRIGHT: &str = "© 2026 Philipp Remy. Freie Software, lizenziert unter der GNU Affero General Public License v3.";
pub const HOMEPAGE: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene";

/// SPDX identifier (`[workspace.package] license`).
pub const LICENSE: &str = "AGPL-3.0-or-later";

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

/// Minimum iOS **SDK** stamped into the built iOS binary's `LC_BUILD_VERSION` (`macos::ensure_min_sdk`,
/// shared with the macOS path). `UIGlassEffect` (Liquid Glass) and the iOS 26 window chrome are gated on
/// the SDK the binary was linked against, and the legacy CI Mac tops out far below iOS 26.
pub const IOS_SDK_FLOOR: &str = "26.0";

/// `CFBundleVersion` / MSI `ProductVersion` want `x.y.z[.b]`; our SemVer string
/// is already in that shape, but strip any pre-release / build metadata.
pub fn numeric_version() -> &'static str {
    VERSION.split(['-', '+']).next().unwrap_or(VERSION)
}
