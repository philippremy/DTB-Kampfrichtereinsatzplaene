//! Build / VCS / toolchain metadata and the embedded dependency licences.
//!
//! Most values come from `build.rs` (via `$OUT_DIR/build_meta.rs`); a few are
//! read from the compiler itself here (`cfg!`). Anything `build.rs` could not
//! determine is the empty string, surfaced as "nicht verfügbar".

/// One dependency and (an index into [`LICENSE_TEXTS`] for) its licence text.
pub struct D {
    pub name: &'static str,
    pub version: &'static str,
    pub spdx: Option<&'static str>,
    pub license: Option<usize>,
}

include!(concat!(env!("OUT_DIR"), "/build_meta.rs"));

// ── compile-time facts not visible to build.rs ────────────────────────────

/// The running build's version. Normally `CARGO_PKG_VERSION` (the workspace
/// `Cargo.toml`), but CI can override it via `DTB_KE_VERSION` — `tip.yml` sets
/// it to `{base}-tip.{run_number}` so the updater can tell consecutive Tip
/// builds apart (they all share the same unchanging base version otherwise, so
/// `0.1.0-tip.35` would never be seen as newer than the installed `0.1.0`).
pub const APP_VERSION: &str = match option_env!("DTB_KE_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
pub const SPDX_LICENSE: &str = env!("CARGO_PKG_LICENSE");
pub const IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzpläne";
pub const DEBUG_ASSERTIONS: bool = cfg!(debug_assertions);
pub const PANIC_STRATEGY: &str = if cfg!(panic = "abort") {
    "abort"
} else {
    "unwind"
};

/// The full AGPL-3.0 text shown by the licence window.
pub const APP_LICENSE_TEXT: &str = include_str!("../assets/AGPL-3.0.txt");

/// The licence text for a dependency, if `build.rs` found one.
pub fn dep_license(d: &D) -> Option<&'static str> {
    d.license.map(|i| LICENSE_TEXTS[i])
}

fn na(s: &str, locale: &crate::i18n::Locale) -> String {
    use crate::i18n::ActiveLocale;
    if s.is_empty() {
        locale.t("about.not-available").to_string()
    } else {
        s.to_string()
    }
}

fn yes_no(b: bool, locale: &crate::i18n::Locale) -> String {
    use crate::i18n::ActiveLocale;
    let key = if b { "about.yes" } else { "about.no" };
    locale.t(key).to_string()
}

/// The metadata rows for the Build-Info window, label + value, in a fixed order.
pub fn rows(locale: &crate::i18n::Locale) -> Vec<(gpui_kit::SharedString, String)> {
    use crate::i18n::ActiveLocale;
    vec![
        (locale.t("about.info-app-version"), APP_VERSION.to_string()),
        (locale.t("about.info-commit"), na(COMMIT, locale)),
        (locale.t("about.info-branch"), na(BRANCH, locale)),
        (locale.t("about.info-commit-date"), na(COMMIT_DATE, locale)),
        (
            locale.t("about.info-working-tree"),
            na(WORKING_TREE, locale),
        ),
        (locale.t("about.info-identifier"), IDENTIFIER.to_string()),
        (
            locale.t("about.info-spdx-license"),
            na(SPDX_LICENSE, locale),
        ),
        (locale.t("about.info-build-profile"), na(PROFILE, locale)),
        (locale.t("about.info-opt-level"), na(OPT_LEVEL, locale)),
        (
            locale.t("about.info-debug-assertions"),
            yes_no(DEBUG_ASSERTIONS, locale),
        ),
        (locale.t("about.info-debug-info"), na(DEBUG_INFO, locale)),
        (locale.t("about.info-lto"), na(LTO, locale)),
        (
            locale.t("about.info-codegen-units"),
            na(CODEGEN_UNITS, locale),
        ),
        (
            locale.t("about.info-panic-strategy"),
            PANIC_STRATEGY.to_string(),
        ),
        (locale.t("about.info-strip"), na(STRIP, locale)),
        (locale.t("about.info-incremental"), na(INCREMENTAL, locale)),
        (
            locale.t("about.info-rust-toolchain"),
            na(RUST_VERSION, locale),
        ),
        (
            locale.t("about.info-llvm-version"),
            na(LLVM_VERSION, locale),
        ),
        (locale.t("about.info-linker"), na(LINKER, locale)),
        (
            locale.t("about.info-target-triple"),
            na(TARGET_TRIPLE, locale),
        ),
        (locale.t("about.info-host-triple"), na(HOST_TRIPLE, locale)),
        (
            locale.t("about.info-dependencies"),
            DEPENDENCY_COUNT.to_string(),
        ),
    ]
}
