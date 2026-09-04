//! Shared SVG geometry + the org-emblem sizing rule.
//!
//! Both the Typst template and the DOCX header size the org emblem by its
//! orientation — a landscape word-mark fits inside a wide/short box, a portrait
//! badge is capped by height. The rule lives here so the two exports stay in
//! step; the caps are [`EMBLEM_LANDSCAPE_W_CM`] etc.

/// Emblem box: a landscape emblem fits inside `W × H`, a portrait one is scaled
/// to `PORTRAIT_H` tall. The landscape **height** cap matters — a near-square
/// "landscape" emblem (Berlin BTFB, aspect ≈ 1.13) otherwise blows up.
pub(crate) const EMBLEM_LANDSCAPE_W_CM: f64 = 6.0;
pub(crate) const EMBLEM_LANDSCAPE_H_CM: f64 = 2.25;
pub(crate) const EMBLEM_PORTRAIT_H_CM: f64 = 2.25;

/// The intrinsic aspect ratio (width / height) of an SVG, or `None` if the
/// bytes don't parse as SVG or have a degenerate size.
fn aspect_ratio(bytes: &[u8]) -> Option<f64> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default()).ok()?;
    let size = tree.size();
    (size.width() > 0.0 && size.height() > 0.0).then(|| (size.width() / size.height()) as f64)
}

/// The on-page emblem frame `(width, height)` for a logo of the given aspect
/// ratio, aspect preserved. Units are whatever the caps are in (cm here, EMU in
/// the DOCX header). Landscape (aspect ≥ 1) fits inside `landscape_w ×
/// landscape_h`; portrait scales to `portrait_h` tall.
pub(crate) fn emblem_frame(
    aspect: f64,
    landscape_w: f64,
    landscape_h: f64,
    portrait_h: f64,
) -> (f64, f64) {
    if aspect >= 1.0 {
        if landscape_w / aspect <= landscape_h {
            (landscape_w, landscape_w / aspect)
        } else {
            (landscape_h * aspect, landscape_h)
        }
    } else {
        (portrait_h * aspect, portrait_h)
    }
}

/// The emblem frame in **cm** for the given SVG, using the shared caps.
/// `None` when the bytes don't parse as SVG (the caller then leaves the emblem
/// out / lets the layout engine pick).
pub(crate) fn emblem_frame_cm(bytes: &[u8]) -> Option<(f64, f64)> {
    let aspect = aspect_ratio(bytes)?;
    Some(emblem_frame(
        aspect,
        EMBLEM_LANDSCAPE_W_CM,
        EMBLEM_LANDSCAPE_H_CM,
        EMBLEM_PORTRAIT_H_CM,
    ))
}
