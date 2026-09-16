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
use std::path::PathBuf;
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

/// Regenerate every icon output from the Icon Composer master. **macOS
/// only** — see the module doc for why there is no fallback path on
/// Windows/Linux. Writes into [`generated_dir`], which is git-tracked;
/// callers are expected to `git add` + commit the result.
pub fn generate() -> Result<bool, String> {
    if !cfg!(target_os = "macos") {
        return Err(
            "icon generation needs Xcode 26's `actool`/`iconutil` (Icon Composer support) — \
             macOS only. Run `cargo dtb-ke-bundle icons` on a Mac with Xcode 26+ and commit \
             assets/icons/generated/."
                .into(),
        );
    }

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
    build_ico(&flat)?;
    build_hicolor(&flat)?;

    Ok(true)
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

/// A square RGBA bitmap `size`×`size`, resized from the flat master.
fn rasterise(master: &DynamicImage, size: u32) -> RgbaImage {
    master
        .resize_exact(size, size, FilterType::Lanczos3)
        .to_rgba8()
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

/// Build a multi-resolution `.ico`. Each entry is a PNG (the ICO format has
/// allowed PNG-compressed images since Windows Vista, and WiX / modern Windows
/// read them fine).
fn build_ico(master: &DynamicImage) -> Result<(), String> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&s| Ok((s, png_bytes(&rasterise(master, s))?)))
        .collect::<Result<_, String>>()?;

    let count = images.len() as u16;
    let mut out = Vec::new();
    // ICONDIR
    out.extend_from_slice(&0u16.to_le_bytes()); // reserved
    out.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    out.extend_from_slice(&count.to_le_bytes());

    // Directory entries are followed by the image data; offsets start after all
    // 16-byte entries.
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 }; // 0 means 256
        out.push(dim); // width
        out.push(dim); // height
        out.push(0); // palette count
        out.push(0); // reserved
        out.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        out.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        out.extend_from_slice(&(png.len() as u32).to_le_bytes());
        out.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in &images {
        out.extend_from_slice(png);
    }
    std::fs::write(ico_path(), out).map_err(io("AppIcon.ico"))
}

// ── Linux hicolor ──────────────────────────────────────────────────────────

fn build_hicolor(master: &DynamicImage) -> Result<(), String> {
    let root = hicolor_dir();
    util::remove(&root).ok();
    for &size in HICOLOR_SIZES {
        let dir = root.join(format!("{size}x{size}")).join("apps");
        std::fs::create_dir_all(&dir).map_err(io("hicolor dir"))?;
        write_png(&rasterise(master, size), &dir.join(format!("{}.png", meta::RDNS_ID)))?;
    }
    // No `scalable/apps/<id>.svg`: unlike the old flat SVG master, a Liquid
    // Glass Icon Composer icon is composited from multiple layers/materials —
    // there's no single self-contained vector to hand freedesktop.
    Ok(())
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> String {
    move |e| format!("{what}: {e}")
}
