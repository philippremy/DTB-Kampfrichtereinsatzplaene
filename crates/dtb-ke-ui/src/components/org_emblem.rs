//! The organization emblem.
//!
//! Renders the real vector logo from `dtb-ke-resource` (`emblems/{slug}.svg`,
//! via `dtb_ke_export::org_slug`) as a height-locked image; falls back to a
//! monogram plate for any organization whose emblem is not committed.
//!
//! The emblems' ink is unified to `#000000` in the asset SVGs; here it is
//! re-tinted to the theme foreground (via `currentColor`) so the wordmarks stay
//! legible in dark mode. Coloured parts of a logo are left untouched.

use std::sync::Arc;

use dtb_ke_types::OrganizationDTO;
use gpui_kit::{
    App, Hsla, Image, ImageFormat, IntoElement, ParentElement, Pixels, RenderOnce, Rgba, Styled,
    Window, div, img, px,
};

use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct OrgEmblem {
    org: OrganizationDTO,
    height: Pixels,
}

impl OrgEmblem {
    pub fn new(org: OrganizationDTO) -> Self {
        Self {
            org,
            height: px(20.),
        }
    }

    pub fn height(mut self, height: impl Into<Pixels>) -> Self {
        self.height = height.into();
        self
    }
}

impl RenderOnce for OrgEmblem {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let slug = dtb_ke_export::org_slug(&self.org);
        if let Some(bytes) = dtb_ke_resource::emblem(&slug) {
            let aspect = svg_aspect(&bytes).unwrap_or(1.0);
            let svg = theme_ink(&bytes, cx.theme().color.foreground);
            let image = Image::from_bytes(ImageFormat::Svg, svg);
            return div()
                .flex()
                .flex_none()
                .items_center()
                .h(self.height)
                .w(self.height * aspect)
                .child(img(Arc::new(image)).size_full())
                .into_any_element();
        }

        // Fallback: a monogram on the emblem plate.
        let theme = cx.theme();
        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .h(self.height)
            .min_w(self.height)
            .px(px(6.))
            .rounded(theme.skin.radius_control_px())
            .border_1()
            .border_color(theme.color.border)
            .bg(theme.color.emblem_plate)
            .text_color(theme.color.emblem_ink)
            .text_size(self.height * 0.42)
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .child(monogram(&self.org))
            .into_any_element()
    }
}

/// Retint an emblem's unified black ink (`#000000`) to `ink`.
///
/// Black fills become `currentColor`, and a `color` presentation attribute is
/// set on the root `<svg>` so usvg resolves it — coloured parts of the logo are
/// left as authored.
fn theme_ink(bytes: &[u8], ink: Hsla) -> Vec<u8> {
    let src = String::from_utf8_lossy(bytes);
    let recoloured = src.replace("#000000", "currentColor");
    match recoloured.find("<svg") {
        Some(i) => {
            let mut out = String::with_capacity(recoloured.len() + 24);
            out.push_str(&recoloured[..i + 4]);
            out.push_str(&format!(" color=\"{}\"", rgb_hex(ink)));
            out.push_str(&recoloured[i + 4..]);
            out.into_bytes()
        }
        None => recoloured.into_bytes(),
    }
}

/// `#rrggbb` for an opaque `Hsla` (alpha dropped — the emblem carries its own).
fn rgb_hex(c: Hsla) -> String {
    let rgba = Rgba::from(c);
    let ch = |f: f32| (f.clamp(0.0, 1.0) * 255.0).round() as u8;
    format!("#{:02x}{:02x}{:02x}", ch(rgba.r), ch(rgba.g), ch(rgba.b))
}

/// width ÷ height from an SVG's `viewBox` (or explicit `width`/`height`).
fn svg_aspect(bytes: &[u8]) -> Option<f32> {
    let head = std::str::from_utf8(bytes.get(..bytes.len().min(600))?).ok()?;
    let after = head.find("viewBox=\"")? + "viewBox=\"".len();
    let inner = &head[after..head[after..].find('"')? + after];
    let mut nums = inner
        .split_whitespace()
        .filter_map(|n| n.parse::<f32>().ok());
    let (_, _, w, h) = (nums.next()?, nums.next()?, nums.next()?, nums.next()?);
    (h > 0.0).then_some(w / h)
}

/// A 2–4 letter code for the organization.
pub fn monogram(org: &OrganizationDTO) -> String {
    match org {
        OrganizationDTO::DTB => "DTB".to_owned(),
        OrganizationDTO::IRV => "IRV".to_owned(),
        OrganizationDTO::LFV(lfv) => initials(&lfv.to_string()),
    }
}

fn initials(name: &str) -> String {
    let code: String = name
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|word| !word.is_empty() && *word != "e.V." && *word != "e.\u{202f}V.")
        .filter_map(|word| word.chars().next())
        .filter(|c| c.is_alphanumeric())
        .take(3)
        .collect();
    code.to_uppercase()
}
