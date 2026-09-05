//! Named colour schemes: a set of semantic roles, each with a light and a dark
//! value, authored as hex strings in TOML.

use gpui::{Hsla, Rgba};
use serde::Deserialize;

use super::{Appearance, ThemeError};

/// A named colour scheme with a light and a dark variant.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub name: String,
    light: RawColors,
    dark: RawColors,
}

impl Palette {
    pub fn parse(toml_src: &str) -> Result<Self, ThemeError> {
        Ok(toml::from_str(toml_src)?)
    }

    /// Resolve the role hex strings for `appearance` into `Hsla`.
    pub fn colors(&self, appearance: Appearance) -> Result<PaletteColors, ThemeError> {
        match appearance {
            Appearance::Light => &self.light,
            Appearance::Dark => &self.dark,
        }
        .resolve()
    }
}

/// Semantic roles as written in TOML (`"#RRGGBB"` strings).
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
struct RawColors {
    background: String,
    surface: String,
    surface_foreground: String,
    foreground: String,
    muted_foreground: String,
    border: String,
    line_strong: String,
    primary: String,
    primary_foreground: String,
    destructive_foreground: String,
    accent_soft: String,
    ring: String,
    ok: String,
    warn: String,
    critical: String,
    selection: String,
    chrome: String,
    overlay: String,
    emblem_plate: String,
    emblem_ink: String,
}

impl RawColors {
    fn resolve(&self) -> Result<PaletteColors, ThemeError> {
        Ok(PaletteColors {
            background: hex(&self.background)?,
            surface: hex(&self.surface)?,
            surface_foreground: hex(&self.surface_foreground)?,
            foreground: hex(&self.foreground)?,
            muted_foreground: hex(&self.muted_foreground)?,
            border: hex(&self.border)?,
            line_strong: hex(&self.line_strong)?,
            primary: hex(&self.primary)?,
            primary_foreground: hex(&self.primary_foreground)?,
            destructive_foreground: hex(&self.destructive_foreground)?,
            accent_soft: hex(&self.accent_soft)?,
            ring: hex(&self.ring)?,
            ok: hex(&self.ok)?,
            warn: hex(&self.warn)?,
            critical: hex(&self.critical)?,
            selection: hex(&self.selection)?,
            chrome: hex(&self.chrome)?,
            overlay: hex(&self.overlay)?,
            emblem_plate: hex(&self.emblem_plate)?,
            emblem_ink: hex(&self.emblem_ink)?,
        })
    }
}

/// Semantic roles resolved to `Hsla`, ready for rendering.
#[derive(Debug, Clone, Copy)]
pub struct PaletteColors {
    /// Window / app ground.
    pub background: Hsla,
    /// Cards and content panes.
    pub surface: Hsla,
    /// Text on [`Self::surface`].
    pub surface_foreground: Hsla,
    /// Text on [`Self::background`].
    pub foreground: Hsla,
    /// Secondary text, captions.
    pub muted_foreground: Hsla,
    /// Hairline borders and dividers.
    pub border: Hsla,
    /// Heavier rules and control outlines.
    pub line_strong: Hsla,
    /// Accent — primary buttons, selection, links.
    pub primary: Hsla,
    /// Text on [`Self::primary`] — white in both themes (the accent is the same
    /// blue in both, so the foreground must be too).
    pub primary_foreground: Hsla,
    /// Text on [`Self::critical`] — separate from [`Self::primary_foreground`]
    /// because the dark palette's `critical` is a light salmon that needs dark
    /// text, while `primary` needs white.
    pub destructive_foreground: Hsla,
    /// Tinted fills (chips, soft highlights).
    pub accent_soft: Hsla,
    /// Focus ring.
    pub ring: Hsla,
    /// Saved / success.
    pub ok: Hsla,
    /// Role conflicts and warnings.
    pub warn: Hsla,
    /// Destructive actions and errors.
    pub critical: Hsla,
    /// Sidebar-row selection base (the skin decides how it's applied).
    pub selection: Hsla,
    /// Toolbar / sidebar surface base.
    pub chrome: Hsla,
    /// Scrim behind a modal dialog (translucent).
    pub overlay: Hsla,
    /// Plate an organization emblem sits on — stays light in *both* themes
    /// (logos are drawn for white backgrounds).
    pub emblem_plate: Hsla,
    /// Text / monogram on [`Self::emblem_plate`].
    pub emblem_ink: Hsla,
}

/// Parse `#rgb`, `#rgba`, `#rrggbb`, or `#rrggbbaa` (leading `#` optional) into
/// `Hsla`.
pub(crate) fn hex(value: &str) -> Result<Hsla, ThemeError> {
    let s = value.trim().strip_prefix('#').unwrap_or(value.trim());
    if !s.is_ascii() {
        return Err(ThemeError::Color(value.to_owned()));
    }

    let expand = |nibble: u8| (nibble << 4) | nibble;
    let nibble = |c: u8| -> Result<u8, ThemeError> {
        (c as char)
            .to_digit(16)
            .map(|d| d as u8)
            .ok_or_else(|| ThemeError::Color(value.to_owned()))
    };
    let byte =
        |pair: &[u8]| -> Result<u8, ThemeError> { Ok((nibble(pair[0])? << 4) | nibble(pair[1])?) };

    let bytes = s.as_bytes();
    let (r, g, b, a) = match bytes.len() {
        3 => (
            expand(nibble(bytes[0])?),
            expand(nibble(bytes[1])?),
            expand(nibble(bytes[2])?),
            255,
        ),
        4 => (
            expand(nibble(bytes[0])?),
            expand(nibble(bytes[1])?),
            expand(nibble(bytes[2])?),
            expand(nibble(bytes[3])?),
        ),
        6 => (
            byte(&bytes[0..2])?,
            byte(&bytes[2..4])?,
            byte(&bytes[4..6])?,
            255,
        ),
        8 => (
            byte(&bytes[0..2])?,
            byte(&bytes[2..4])?,
            byte(&bytes[4..6])?,
            byte(&bytes[6..8])?,
        ),
        _ => return Err(ThemeError::Color(value.to_owned())),
    };

    Ok(Hsla::from(Rgba {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: a as f32 / 255.0,
    }))
}
