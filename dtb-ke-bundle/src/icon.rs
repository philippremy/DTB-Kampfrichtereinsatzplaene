//! Icon pipeline: one master image → `.icns` (macOS), `.ico` (Windows) and a
//! freedesktop hicolor PNG set (Linux).
//!
//! The master lives at the workspace root — `assets/icons/AppIcon.svg`
//! (preferred) or a square `assets/icons/AppIcon.png` of at least 1024×1024 —
//! shared with `dtb-ke-ui` (which embeds the PNG for its About window).
//! Everything else is derived and cached under `target/bundle/icon/`.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use image::{DynamicImage, ImageFormat, RgbaImage, imageops::FilterType};

use crate::meta;
use crate::util::workspace_root;

/// The cache directory for derived icons.
pub fn cache_dir() -> PathBuf {
    workspace_root().join("target/bundle/icon")
}

pub fn icns_path() -> PathBuf {
    cache_dir().join("AppIcon.icns")
}
pub fn ico_path() -> PathBuf {
    cache_dir().join("AppIcon.ico")
}
pub fn hicolor_dir() -> PathBuf {
    cache_dir().join("hicolor")
}
/// A plain 512×512 PNG (AppImage `.DirIcon`, generic use).
pub fn png_512_path() -> PathBuf {
    cache_dir().join("icon-512.png")
}
/// The master SVG, copied verbatim (only when the master is SVG).
pub fn scalable_svg_path() -> PathBuf {
    cache_dir().join("icon.svg")
}

/// The freedesktop icon sizes we ship (px). `scalable/` carries the SVG too.
const HICOLOR_SIZES: &[u32] = &[16, 22, 24, 32, 48, 64, 96, 128, 192, 256, 512];
/// Sizes packed into the multi-resolution `.ico`.
const ICO_SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

/// Generate every icon output from the master. Returns `Ok(false)` if no master
/// is present (callers may proceed without an icon).
pub fn generate() -> Result<bool, String> {
    let Some(master) = Master::find()? else {
        return Ok(false);
    };
    std::fs::create_dir_all(cache_dir()).map_err(io("icon cache dir"))?;

    if let Master::Svg(bytes) = &master {
        std::fs::write(scalable_svg_path(), bytes).map_err(io("scalable icon"))?;
    }
    write_png(&master.rasterise(512)?, &png_512_path())?;
    build_icns(&master)?;
    build_ico(&master)?;
    build_hicolor(&master)?;
    Ok(true)
}

/// The master art, in whichever form it was provided.
enum Master {
    Svg(Vec<u8>),
    Raster(DynamicImage),
}

impl Master {
    /// Look for `assets/icons/AppIcon.{svg,png}` at the workspace root (svg wins).
    fn find() -> Result<Option<Self>, String> {
        let dir = workspace_root().join("assets/icons");
        let svg = dir.join("AppIcon.svg");
        let png = dir.join("AppIcon.png");
        if svg.exists() {
            let bytes = std::fs::read(&svg).map_err(io("AppIcon.svg"))?;
            return Ok(Some(Master::Svg(bytes)));
        }
        if png.exists() {
            let img = image::open(&png).map_err(|e| format!("AppIcon.png: {e}"))?;
            let (w, h) = (img.width(), img.height());
            if w != h {
                eprintln!(
                    "dtb-ke-bundle: warning — AppIcon.png is {w}×{h}, not square; it will be stretched"
                );
            }
            if w < 1024 {
                eprintln!(
                    "dtb-ke-bundle: warning — AppIcon.png is only {w}px; 1024px or an SVG is recommended"
                );
            }
            return Ok(Some(Master::Raster(img)));
        }
        eprintln!(
            "dtb-ke-bundle: no icon master — drop `AppIcon.svg` or `AppIcon.png` into \
             assets/icons/ (see assets/README.md); bundling without an icon"
        );
        Ok(None)
    }

    /// A square RGBA bitmap `size`×`size`.
    fn rasterise(&self, size: u32) -> Result<RgbaImage, String> {
        match self {
            Master::Raster(img) => Ok(img
                .resize_exact(size, size, FilterType::Lanczos3)
                .to_rgba8()),
            Master::Svg(bytes) => render_svg(bytes, size),
        }
    }
}

/// Rasterise an SVG into a `size`×`size` RGBA bitmap, fitting the artwork
/// centred with its aspect preserved.
fn render_svg(svg: &[u8], size: u32) -> Result<RgbaImage, String> {
    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg, &options).map_err(|e| format!("icon SVG: {e}"))?;
    let svg_size = tree.size();
    let (sw, sh) = (svg_size.width(), svg_size.height());
    if sw <= 0.0 || sh <= 0.0 {
        return Err("icon SVG has zero size".into());
    }

    let scale = (size as f32 / sw).min(size as f32 / sh);
    let (dw, dh) = (sw * scale, sh * scale);
    let (tx, ty) = ((size as f32 - dw) / 2.0, (size as f32 - dh) / 2.0);

    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)
        .ok_or_else(|| "icon: pixmap allocation failed".to_string())?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(tx, ty),
        &mut pixmap.as_mut(),
    );

    RgbaImage::from_raw(size, size, pixmap.take())
        .ok_or_else(|| "icon: pixmap → image conversion failed".to_string())
}

/// PNG-encode an RGBA bitmap.
fn png_bytes(img: &RgbaImage) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    DynamicImage::ImageRgba8(img.clone())
        .write_to(&mut Cursor::new(&mut out), ImageFormat::Png)
        .map_err(|e| format!("PNG encode: {e}"))?;
    Ok(out)
}

fn write_png(img: &RgbaImage, path: &Path) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io("icon dir"))?;
    }
    std::fs::write(path, png_bytes(img)?).map_err(io("icon PNG"))
}

// ── macOS .icns ─────────────────────────────────────────────────────────────

/// Build `AppIcon.icns` via `iconutil` from a generated `.iconset` directory.
/// (`iconutil` is a macOS built-in; the `.app` is only ever bundled on macOS.)
fn build_icns(master: &Master) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        eprintln!("dtb-ke-bundle: skipping .icns (iconutil is macOS-only)");
        return Ok(());
    }
    let set = cache_dir().join("AppIcon.iconset");
    crate::util::fresh_dir(&set).map_err(io("iconset dir"))?;

    // (base size, retina?) → file name `icon_<b>x<b>[@2x].png`
    for &(base, retina) in &[
        (16u32, false),
        (16, true),
        (32, false),
        (32, true),
        (128, false),
        (128, true),
        (256, false),
        (256, true),
        (512, false),
        (512, true),
    ] {
        let px = if retina { base * 2 } else { base };
        let name = if retina {
            format!("icon_{base}x{base}@2x.png")
        } else {
            format!("icon_{base}x{base}.png")
        };
        write_png(&master.rasterise(px)?, &set.join(name))?;
    }

    crate::util::try_run(
        "iconutil",
        &[
            "-c",
            "icns",
            &set.to_string_lossy(),
            "-o",
            &icns_path().to_string_lossy(),
        ],
        &workspace_root(),
    )?;
    crate::util::remove(&set).ok();
    Ok(())
}

// ── Windows .ico ────────────────────────────────────────────────────────────

/// Build a multi-resolution `.ico`. Each entry is a PNG (the ICO format has
/// allowed PNG-compressed images since Windows Vista, and WiX / modern Windows
/// read them fine).
fn build_ico(master: &Master) -> Result<(), String> {
    let images: Vec<(u32, Vec<u8>)> = ICO_SIZES
        .iter()
        .map(|&s| Ok((s, png_bytes(&master.rasterise(s)?)?)))
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

fn build_hicolor(master: &Master) -> Result<(), String> {
    let root = hicolor_dir();
    crate::util::remove(&root).ok();
    for &size in HICOLOR_SIZES {
        let dir = root.join(format!("{size}x{size}")).join("apps");
        std::fs::create_dir_all(&dir).map_err(io("hicolor dir"))?;
        write_png(
            &master.rasterise(size)?,
            &dir.join(format!("{}.png", meta::IDENTIFIER)),
        )?;
    }
    if let Master::Svg(bytes) = master {
        let dir = root.join("scalable").join("apps");
        std::fs::create_dir_all(&dir).map_err(io("hicolor scalable dir"))?;
        std::fs::write(dir.join(format!("{}.svg", meta::IDENTIFIER)), bytes)
            .map_err(io("scalable icon"))?;
    }
    Ok(())
}

fn io(what: &'static str) -> impl Fn(std::io::Error) -> String {
    move |e| format!("{what}: {e}")
}
