//! Icon pipeline: the master is an Xcode 26 **Icon Composer** package —
//! `assets/icons/AppIcon.icon`, a `.icon` bundle of `icon.json` (layers,
//! materials, the Liquid Glass "glass"/shadow/translucency settings) plus its
//! source images — compiled by Apple's own `actool`/`iconutil` (no portable
//! equivalent; Icon Composer's rendering isn't something `resvg`/`image` can
//! reproduce) into every shipped platform icon:
//!
//! - `AppIcon.icns` — a fully rasterised, multi-size **standalone** icon,
//!   flattened out of the Liquid Glass layers. This is the pre-Tahoe
//!   `CFBundleIconFile` fallback (see `macos.rs`).
//! - `Assets.car` — the compiled asset catalog carrying the *actual* Liquid
//!   Glass icon (materials, glass layers, translucency) that Tahoe+ Finder
//!   renders. Embedded at `Contents/Resources/Assets.car`; looked up via
//!   `CFBundleIconName` (must equal [`APP_ICON_NAME`], the `--app-icon` value
//!   below — same two-key split as e.g. Blender's `.app`:
//!   `CFBundleIconFile` → `..._legacy.icns`, `CFBundleIconName` → the
//!   Liquid Glass asset name).
//! - `AppIcon.png` — a flat 1024×1024 raster, extracted from the fallback
//!   `.icns`'s largest rendition. The one bridge back to plain pixels: every
//!   other derivation (Windows `.ico`, Linux hicolor PNGs, `dtb-ke-ui`'s
//!   About window) is pure Rust (`image`) working from this file, so only
//!   *this* step needs macOS tooling.
//!
//! Because Icon Composer icons can only be compiled on a Mac with Xcode 26+
//! (`actool`'s `--standalone-icon-behavior`/Liquid Glass support), unlike the
//! old flat SVG/PNG master this can't be regenerated on every host at build
//! time. So the outputs are **committed to `assets/icons/generated/`** as
//! regular, git-tracked files rather than a `target/`-cached artifact:
//! `cargo dtb-ke-bundle bundle` on Windows/Linux just consumes what's already
//! checked in ([`available`]); only `cargo dtb-ke-bundle icons`, macOS-only,
//! regenerates them — run it and commit the result whenever
//! `assets/icons/AppIcon.icon` changes.

use std::io::Cursor;
use std::path::{Path, PathBuf};
use std::process::Command;

use image::{DynamicImage, ImageFormat, RgbaImage, imageops::FilterType};

use crate::meta;
use crate::util::{self, workspace_root};

/// The Icon Composer master package.
fn master_dir() -> PathBuf {
    workspace_root().join("assets/icons/AppIcon.icon")
}

/// Where every derived, git-tracked icon output lives.
pub fn generated_dir() -> PathBuf {
    workspace_root().join("assets/icons/generated")
}

/// A scratch directory for `actool`/`iconutil`'s own intermediate output —
/// deliberately *outside* [`generated_dir`] so a crash mid-generation can
/// never leave stray files for `git add` to pick up.
fn scratch_dir() -> PathBuf {
    workspace_root().join("target/bundle/icon-scratch")
}

/// The name passed to `actool --app-icon`, and thus the compiled asset's
/// name inside `Assets.car` — must equal `CFBundleIconName` in `macos.rs`.
pub const APP_ICON_NAME: &str = "AppIcon";

pub fn icns_path() -> PathBuf {
    generated_dir().join("AppIcon.icns")
}
pub fn assets_car_path() -> PathBuf {
    generated_dir().join("Assets.car")
}
/// The flat 1024×1024 raster master (see the module doc).
pub fn flat_png_path() -> PathBuf {
    generated_dir().join("AppIcon.png")
}
pub fn ico_path() -> PathBuf {
    generated_dir().join("AppIcon.ico")
}
pub fn hicolor_dir() -> PathBuf {
    generated_dir().join("hicolor")
}
/// A plain 512×512 PNG (AppImage `.DirIcon`, generic use).
pub fn png_512_path() -> PathBuf {
    generated_dir().join("icon-512.png")
}

/// The `.dtbke` file-type icon master — a flat, square RGBA PNG (≥1024×1024
/// recommended), unlike [`master_dir`] committed by hand rather than
/// authored in Icon Composer: a document icon needs no Liquid Glass
/// materials/layering, so it's rasterised straight into every output the
/// same way [`flat_png_path`] itself is (see the module doc's "one bridge
/// back to plain pixels" note) — only the macOS `.icns` step actually needs
/// to run on a Mac (`iconutil`; no `actool`/Icon Composer involved), the
/// `.ico` and hicolor PNGs are pure Rust wherever [`generate`] runs. Still
/// folded into the same macOS-only `generate()` / `cargo dtb-ke-bundle
/// icons` step as the app icon, for one regenerate-and-commit story rather
/// than two.
///
/// Unlike [`master_dir`], this is expected to be **non-square** — a document
/// icon is conventionally a page/leaf silhouette, not a squircle — so every
/// derivation below fits it into each target size preserving aspect ratio
/// (transparent letterboxing) rather than stretching it, see
/// [`rasterise_contained`].
pub fn doc_master_path() -> PathBuf {
    workspace_root().join("assets/icons/FileIcon.png")
}
pub fn doc_icns_path() -> PathBuf {
    generated_dir().join("DocumentIcon.icns")
}
pub fn doc_ico_path() -> PathBuf {
    generated_dir().join("DocumentIcon.ico")
}
/// The freedesktop mimetype icon name for `.dtbke` —
/// `meta::DOC_MIME_TYPE` with `/` → `-`, the lookup convention icon themes
/// use for a MIME type with no explicit `<icon>` override in the
/// shared-mime-info package (see `linux.rs`).
pub const DOC_MIME_ICON_NAME: &str = "application-x-dtbke";

/// Whether the committed document-type icons are present.
pub fn doc_available() -> bool {
    doc_ico_path().exists()
}

/// The freedesktop icon sizes we ship (px).
const HICOLOR_SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 512];
/// Sizes packed into the multi-resolution `.ico`.
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// Whether the committed generated icons are present. Checked by `bundle` on
/// every host — Windows/Linux never regenerate, they only consume what
/// [`generate`] (macOS-only) already wrote and a human committed.
pub fn available() -> bool {
    flat_png_path().exists()
}

/// Regenerate every icon output — the app icon from the Icon Composer
/// master, and the `.dtbke` document-type icon from its own flat master, if
/// committed (see [`doc_master_path`]). **macOS only** — see the module doc
/// for why there is no fallback path on Windows/Linux (the app icon's
/// `actool` step needs it; the document icon's `iconutil` step is folded in
/// here too for one regenerate-and-commit story). Writes into
/// [`generated_dir`], which is git-tracked; callers are expected to `git
/// add` + commit the result.
pub fn generate() -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Err(
            "icon generation needs Xcode 26's `actool`/`iconutil` (Icon Composer support) — \
             macOS only. Run `cargo dtb-ke-bundle icons` on a Mac with Xcode 26+ and commit \
             assets/icons/generated/."
                .into(),
        );
    }

    let app_icon = generate_app_icon()?;
    let doc_icon = generate_doc_icon()?;
    Ok(app_icon || doc_icon)
}

/// The app-icon half of [`generate`] — the Icon Composer master → `.icns` /
/// `Assets.car` / `.ico` / hicolor `apps/` PNGs. `Ok(false)` (no error) when
/// there's no master to build from yet.
fn generate_app_icon() -> Result<bool, String> {
    let master = master_dir();
    if !master.exists() {
        eprintln!(
            "dtb-ke-bundle: no icon master at {} — bundling without an icon",
            master.display()
        );
        return Ok(false);
    }

    let scratch = scratch_dir();
    util::fresh_dir(&scratch).map_err(io("icon scratch dir"))?;

    run_actool(&master, &scratch)?;

    let out_dir = generated_dir();
    util::fresh_dir(&out_dir).map_err(io("icon output dir"))?;
    std::fs::rename(scratch.join("AppIcon.icns"), icns_path()).map_err(io("AppIcon.icns"))?;
    std::fs::rename(scratch.join("Assets.car"), assets_car_path()).map_err(io("Assets.car"))?;

    let flat = extract_flat_png(&scratch)?;
    util::remove(&scratch).ok();

    write_png(&flat.resize_exact(512, 512, FilterType::Lanczos3).to_rgba8(), &png_512_path())?;
    build_ico(&flat, &ico_path())?;
    build_hicolor_named(&flat, "apps", meta::RDNS_ID)?;

    Ok(true)
}

/// The document-icon half of [`generate`] — [`doc_master_path`] → `.icns` /
/// `.ico` / hicolor `mimetypes/` PNGs. `Ok(false)` (no error) when there's no
/// master to build from yet — the icon is expected to be added later, and
/// bundling without one just means the OS shows a generic file icon for
/// `.dtbke` (see the `#cfg` sites in `macos.rs`/`windows.rs`/`linux.rs`).
fn generate_doc_icon() -> Result<bool, String> {
    let master_path = doc_master_path();
    if !master_path.exists() {
        eprintln!(
            "dtb-ke-bundle: no document-type icon master at {} — bundling without a \
             .dtbke file-type icon",
            master_path.display()
        );
        return Ok(false);
    }
    let master =
        image::open(&master_path).map_err(|e| format!("opening {}: {e}", master_path.display()))?;

    std::fs::create_dir_all(generated_dir()).map_err(io("icon output dir"))?;
    build_icns_from_flat(&master, &doc_icns_path())?;
    build_ico(&master, &doc_ico_path())?;
    build_hicolor_named(&master, "mimetypes", DOC_MIME_ICON_NAME)?;

    Ok(true)
}

/// Build an `.icns` from a flat master image via a hand-built `.iconset` —
/// the reverse of [`extract_flat_png`]. Only needs `iconutil` (ships with
/// the Xcode Command Line Tools), unlike the app icon's `actool`/Icon
/// Composer pipeline.
fn build_icns_from_flat(master: &DynamicImage, out: &Path) -> Result<(), String> {
    let scratch = scratch_dir().join("doc-icon-src");
    util::fresh_dir(&scratch).map_err(io("doc icon scratch dir"))?;
    let iconset = scratch.join("DocumentIcon.iconset");
    std::fs::create_dir_all(&iconset).map_err(io("iconset dir"))?;

    // Apple's `.iconset` naming convention: (pixel size, file name).
    const ICNS_SIZES: &[(u32, &str)] = &[
        (16, "icon_16x16.png"),
        (32, "icon_16x16@2x.png"),
        (32, "icon_32x32.png"),
        (64, "icon_32x32@2x.png"),
        (128, "icon_128x128.png"),
        (256, "icon_128x128@2x.png"),
        (256, "icon_256x256.png"),
        (512, "icon_256x256@2x.png"),
        (512, "icon_512x512.png"),
        (1024, "icon_512x512@2x.png"),
    ];
    for (size, name) in ICNS_SIZES {
        write_png(&rasterise(master, *size), &iconset.join(name))?;
    }

    util::try_run(
        "iconutil",
        &[
            "--convert",
            "icns",
            "--output",
            &out.to_string_lossy(),
            &iconset.to_string_lossy(),
        ],
        &workspace_root(),
    )
    .map_err(|e| format!("iconutil: {e}"))?;
    util::remove(&scratch).ok();
    Ok(())
}

/// Compile the `.icon` master into `scratch/{AppIcon.icns,Assets.car}`.
///
/// `actool` can report success (exit 0) while writing nothing at all — e.g.
/// omitting `--output-partial-info-plist` degrades app-icon compilation to a
/// silent no-op "notice" rather than an error — so the real check is that the
/// two expected output files actually landed, not just the exit status.
/// `--standalone-icon-behavior all` is required for a *complete* fallback
/// `.icns`: the default only emits a couple of representative sizes (16/128),
/// not something fit to ship as the pre-Tahoe icon.
fn run_actool(master: &std::path::Path, scratch: &std::path::Path) -> Result<(), String> {
    let partial_plist = scratch.join("partial.plist");
    let output = Command::new("actool")
        .args([
            "--compile",
            &scratch.to_string_lossy(),
            "--platform",
            "macosx",
            "--minimum-deployment-target",
            meta::MACOS_MIN_VERSION,
            "--app-icon",
            APP_ICON_NAME,
            "--standalone-icon-behavior",
            "all",
            "--errors",
            "--warnings",
            "--notices",
            "--output-partial-info-plist",
        ])
        .arg(&partial_plist)
        .arg(master)
        .current_dir(workspace_root())
        .output()
        .map_err(|e| format!("failed to spawn actool: {e}"))?;

    if !output.status.success() || !scratch.join("AppIcon.icns").exists() {
        return Err(format!(
            "actool did not produce AppIcon.icns/Assets.car (exit {}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout)
        ));
    }
    Ok(())
}

/// Convert the fallback `.icns` to an `.iconset` and pull out its largest
/// (1024×1024, `icon_512x512@2x.png`) rendition as a flat `DynamicImage`.
fn extract_flat_png(scratch: &std::path::Path) -> Result<DynamicImage, String> {
    let iconset = scratch.join("AppIcon.iconset");
    util::try_run(
        "iconutil",
        &[
            "--convert",
            "iconset",
            "--output",
            &iconset.to_string_lossy(),
            &icns_path().to_string_lossy(),
        ],
        &workspace_root(),
    )
    .map_err(|e| format!("iconutil: {e}"))?;

    let largest = iconset.join("icon_512x512@2x.png");
    std::fs::copy(&largest, flat_png_path()).map_err(io("flat AppIcon.png"))?;
    image::open(&largest).map_err(|e| format!("re-reading extracted AppIcon.png: {e}"))
}

/// A square RGBA bitmap `size`×`size`: `master` scaled to fit within the
/// square (preserving aspect ratio) and centred on a transparent
/// background. For a square master — the app icon's flattened Icon Composer
/// output — the fit is exact and this is returned straight from
/// `resize_exact`, byte-for-byte what a plain resize would give (compositing
/// unconditionally, even when nothing needs letterboxing, would alpha-blend
/// every partially-transparent edge pixel onto the canvas and premultiply
/// its RGB — a real, if subtle, corruption of the app icon's anti-aliased
/// edges). For a non-square one — the document icon's page silhouette, see
/// [`doc_master_path`] — it letterboxes instead of stretching.
fn rasterise(master: &DynamicImage, size: u32) -> RgbaImage {
    let (w, h) = (master.width(), master.height());
    let scale = (f64::from(size) / f64::from(w)).min(f64::from(size) / f64::from(h));
    let new_w = ((f64::from(w) * scale).round() as u32).clamp(1, size);
    let new_h = ((f64::from(h) * scale).round() as u32).clamp(1, size);
    let resized = master.resize_exact(new_w, new_h, FilterType::Lanczos3).to_rgba8();
    if new_w == size && new_h == size {
        return resized;
    }

    let mut canvas = RgbaImage::new(size, size);
    let x_off = i64::from((size - new_w) / 2);
    let y_off = i64::from((size - new_h) / 2);
    image::imageops::overlay(&mut canvas, &resized, x_off, y_off);
    canvas
}

/// PNG-encode an RGBA bitmap.
fn png_bytes(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(out)
}

fn write_png(img: &RgbaImage, path: &std::path::Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io("icon dir"))?;
    }
    std::fs::write(path, png_bytes(img)?).map_err(io("icon PNG"))
}

// ── Windows .ico ────────────────────────────────────────────────────────────

/// Build a multi-resolution `.ico` at `out`. Each entry is a PNG (the ICO
/// format has allowed PNG-compressed images since Windows Vista, and WiX /
/// modern Windows read them fine).
fn build_ico(master: &DynamicImage, out: &Path) -> Result<(), String> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&s| Ok((s, png_bytes(&rasterise(master, s))?)))
        .collect::<Result<_, String>>()?;

    let count = images.len() as u16;
    let mut buf = Vec::new();
    // ICONDIR
    buf.extend_from_slice(&0u16.to_le_bytes()); // reserved
    buf.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    buf.extend_from_slice(&count.to_le_bytes());

    // Directory entries are followed by the image data; offsets start after all
    // 16-byte entries.
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 }; // 0 means 256
        buf.push(dim); // width
        buf.push(dim); // height
        buf.push(0); // palette count
        buf.push(0); // reserved
        buf.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        buf.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        buf.extend_from_slice(&(png.len() as u32).to_le_bytes());
        buf.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in &images {
        buf.extend_from_slice(png);
    }
    std::fs::write(out, buf).map_err(io("writing .ico"))
}

// ── Linux hicolor ──────────────────────────────────────────────────────────

/// Write `master` into every [`HICOLOR_SIZES`] rendition under
/// `hicolor_dir()/<size>x<size>/<category>/<name>.png` — `category` is
/// `"apps"` (app icon, name = `meta::RDNS_ID`) or `"mimetypes"` (the
/// `.dtbke` file-type icon, name = [`DOC_MIME_ICON_NAME`]). Doesn't touch
/// any other category already on disk — callers that want a clean rebuild
/// wipe `hicolor_dir()` themselves first (see `generate_app_icon`, which
/// runs first and freshens the whole `generated/` tree).
fn build_hicolor_named(master: &DynamicImage, category: &str, name: &str) -> Result<(), String> {
    let root = hicolor_dir();
    for &size in HICOLOR_SIZES {
        let dir = root.join(format!("{size}x{size}")).join(category);
        std::fs::create_dir_all(&dir).map_err(io("hicolor dir"))?;
        write_png(&rasterise(master, size), &dir.join(format!("{name}.png")))?;
    }
    // No `scalable/…/<name>.svg`: unlike the old flat SVG master, a Liquid
    // Glass Icon Composer icon (and, for the document icon, its own flat
    // raster master) has no single self-contained vector to hand freedesktop.
    Ok(())
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> String {
    move |e| format!("{what}: {e}")
}
