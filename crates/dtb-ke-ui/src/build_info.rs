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

fn na(s: &str) -> String {
    if s.is_empty() {
        "nicht verfügbar".to_string()
    } else {
        s.to_string()
    }
}

fn yes_no(b: bool) -> String {
    if b { "ja" } else { "nein" }.to_string()
}

/// The metadata rows for the Build-Info window, label + value, in a fixed order.
pub fn rows() -> Vec<(&'static str, String)> {
    vec![
        ("App-Version", APP_VERSION.to_string()),
        ("Commit", na(COMMIT)),
        ("Branch", na(BRANCH)),
        ("Commit-Datum", na(COMMIT_DATE)),
        ("Arbeitsverzeichnis", na(WORKING_TREE)),
        ("Kennung", IDENTIFIER.to_string()),
        ("SPDX-Lizenz", na(SPDX_LICENSE)),
        ("Build-Profil", na(PROFILE)),
        ("Optimierungsstufe", na(OPT_LEVEL)),
        ("Debug-Assertions", yes_no(DEBUG_ASSERTIONS)),
        ("Debug-Informationen", na(DEBUG_INFO)),
        ("LTO", na(LTO)),
        ("Codegen-Units", na(CODEGEN_UNITS)),
        ("Panic-Strategie", PANIC_STRATEGY.to_string()),
        ("Stripping", na(STRIP)),
        ("Inkrementell", na(INCREMENTAL)),
        ("Rust-Toolchain", na(RUST_VERSION)),
        ("LLVM-Version", na(LLVM_VERSION)),
        ("Linker", na(LINKER)),
        ("Ziel-Triple", na(TARGET_TRIPLE)),
        ("Host-Triple", na(HOST_TRIPLE)),
        ("Abhängigkeiten", DEPENDENCY_COUNT.to_string()),
    ]
}
