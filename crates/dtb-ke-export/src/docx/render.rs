//! SVG → PNG rasterisation for the DOCX header logos.
//!
//! Word's picture model wants a raster blip; modern Word additionally renders
//! an `asvg:svgBlip` extension when present. So each header logo ships **twice**
//! — the original SVG (for Word 2016+) and a PNG rendered here (the universal
//! fallback, and what LibreOffice shows). The SVG bytes are kept verbatim;
//! only the PNG is produced.

use crate::ExportError;

/// A header logo, ready to embed: the source SVG, a rendered PNG fallback, and
/// the on-page frame size in EMU (aspect always preserved).
pub struct RenderedLogo {
    pub svg: Vec<u8>,
    pub png: Vec<u8>,
    pub cx: i64,
    pub cy: i64,
}

/// How to size a logo on the page (aspect is always preserved).
#[derive(Clone, Copy)]
pub enum SizePolicy {
    /// Fit inside a `w` × `h` EMU box (Typst's `fit: "contain"`).
    Contain { w: i64, h: i64 },
    /// Orientation-dependent: a landscape logo (aspect ≥ 1) fits inside
    /// `landscape_w` × `landscape_h` (so a near-square "landscape" emblem is
    /// still bounded vertically); a portrait one is scaled to `portrait_h`
    /// tall.
    ByOrientation {
        landscape_w: i64,
        landscape_h: i64,
        portrait_h: i64,
    },
}

/// Render dots-per-inch for the PNG fallback. High enough that the logo stays
/// crisp when printed at its on-page size.
const RENDER_DPI: f64 = 600.0;
const EMU_PER_INCH: f64 = 914_400.0;

/// Rasterise `svg` and size it per `policy`. Returns `None` (rather than
/// erroring) if the bytes are not SVG — the header logos are all SVG, so a
/// non-SVG asset just means "no header art".
pub fn rasterise(svg: &[u8], policy: SizePolicy) -> Result<Option<RenderedLogo>, ExportError> {
    if !looks_like_svg(svg) {
        return Ok(None);
    }

    let options = resvg::usvg::Options::default();
    let tree = resvg::usvg::Tree::from_data(svg, &options)
        .map_err(|e| ExportError::Template(format!("DOCX header SVG: {e}")))?;

    let size = tree.size();
    if size.width() <= 0.0 || size.height() <= 0.0 {
        return Ok(None);
    }
    let aspect = (size.width() / size.height()) as f64;

    let (cx, cy) = match policy {
        SizePolicy::Contain { w, h } => {
            if (w as f64 / aspect) <= h as f64 {
                (w, (w as f64 / aspect).round() as i64)
            } else {
                ((h as f64 * aspect).round() as i64, h)
            }
        }
        SizePolicy::ByOrientation {
            landscape_w,
            landscape_h,
            portrait_h,
        } => {
            let (w, h) = crate::svg::emblem_frame(
                aspect,
                landscape_w as f64,
                landscape_h as f64,
                portrait_h as f64,
            );
            (w.round() as i64, h.round() as i64)
        }
    };

    let px_h = ((cy as f64 / EMU_PER_INCH) * RENDER_DPI).round().max(1.0);
    let px_w = (px_h * aspect).round().max(1.0);
    let (px_w, px_h) = (px_w as u32, px_h as u32);

    let scale = px_h as f32 / size.height();
    let mut pixmap = resvg::tiny_skia::Pixmap::new(px_w, px_h)
        .ok_or_else(|| ExportError::Template("DOCX header: pixmap allocation failed".into()))?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::from_scale(scale, scale),
        &mut pixmap.as_mut(),
    );

    let png = pixmap
        .encode_png()
        .map_err(|e| ExportError::Template(format!("DOCX header PNG: {e}")))?;

    Ok(Some(RenderedLogo {
        svg: svg.to_vec(),
        png,
        cx,
        cy,
    }))
}

/// A cheap sniff: SVG is XML whose first tag is `<svg` (optionally after a
/// declaration, BOM or whitespace). Good enough to tell our vector emblems from
/// a stray PNG/JPEG.
fn looks_like_svg(bytes: &[u8]) -> bool {
    let head = &bytes[..bytes.len().min(512)];
    let text = String::from_utf8_lossy(head);
    let text = text.trim_start_matches('\u{feff}').trim_start();
    text.starts_with("<?xml") || text.starts_with("<!--") || text.contains("<svg")
}
