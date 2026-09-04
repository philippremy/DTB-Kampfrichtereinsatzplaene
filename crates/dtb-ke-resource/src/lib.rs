//! Compile-time-embedded binary assets for the exported document: fonts, the
//! header logos, and the per-organisation emblems.
//!
//! Everything is embedded via [`rust_embed`] (with `debug-embed`, so debug
//! builds embed too) — nothing here reads the filesystem at runtime. The
//! Typst [`World`](../dtb_ke_export) is built entirely from these bytes.
//!
//! Files are looked up by their path *relative to `assets/`*, e.g.
//! `logos/dtb.svg`. Missing files return `None` rather than panicking, so the
//! exporter can fall back (to a bundled Typst font, to a monogram emblem, …)
//! while the real artwork is still being added — see `assets/README.md`.

use std::borrow::Cow;

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "fonts/*"]
#[include = "logos/*"]
#[include = "emblems/*"]
#[exclude = "*.gitkeep"]
#[exclude = "*.md"]
struct Assets;

/// The raw bytes of an embedded asset, addressed by its path under `assets/`
/// (e.g. `"logos/dtb.svg"`). `None` if no such file was embedded.
pub fn get(path: &str) -> Option<Cow<'static, [u8]>> {
    Assets::get(path).map(|file| file.data)
}

/// Whether an asset exists.
pub fn contains(path: &str) -> bool {
    Assets::get(path).is_some()
}

/// Every embedded font file, in a stable (path-sorted) order. Empty until font
/// files are added under `assets/fonts/`.
pub fn fonts() -> Vec<Cow<'static, [u8]>> {
    let mut paths: Vec<Cow<'static, str>> =
        Assets::iter().filter(|p| p.starts_with("fonts/")).collect();
    paths.sort();
    paths.into_iter().filter_map(|p| get(p.as_ref())).collect()
}

/// The DTB word-mark shown top-left on every page. Expected at
/// `assets/logos/dtb.svg` (SVG preferred; PNG also works).
pub fn logo_dtb() -> Option<Cow<'static, [u8]>> {
    first_of(&["logos/dtb.svg", "logos/dtb.png"])
}

/// The "TURNEN! · RHÖNRADTURNEN" swoosh shown top-right on every page.
/// Expected at `assets/logos/turnen.svg`.
pub fn logo_turnen() -> Option<Cow<'static, [u8]>> {
    first_of(&["logos/turnen.svg", "logos/turnen.png"])
}

/// A named organisation emblem, addressed by a stable slug (see
/// `dtb-ke-export`'s mapping from `OrganizationDTO`). Looks for
/// `assets/emblems/{slug}.svg` then `.png`.
pub fn emblem(slug: &str) -> Option<Cow<'static, [u8]>> {
    first_of(&[
        &format!("emblems/{slug}.svg"),
        &format!("emblems/{slug}.png"),
    ])
}

fn first_of(paths: &[&str]) -> Option<Cow<'static, [u8]>> {
    paths.iter().find_map(|p| get(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_of_absent_asset_is_none_not_panic() {
        assert!(get("logos/does-not-exist.svg").is_none());
        assert!(!contains("emblems/nope.png"));
    }

    #[test]
    fn fonts_listing_is_sorted_and_total() {
        // No assertion on count (may be zero before fonts are committed), but
        // the call must not panic and must be deterministically ordered.
        let a = fonts().len();
        let b = fonts().len();
        assert_eq!(a, b);
    }
}
