//! Which platform visual language to present, and the structural metrics that
//! go with it (colours live in the palette, see [`super::palette`]).

use gpui_kit::{Pixels, SharedString, px};
use serde::Deserialize;

use super::ThemeError;

/// The platform skin. Chosen once at startup by [`Skin::detect`]; overridable
/// with the `DTB_KE_SKIN` env var (`mac` | `win` | `linux`) for previewing one
/// skin on another OS.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Skin {
    MacLiquidGlass,
    WinUi3,
    LinuxNeutral,
}

impl Skin {
    pub fn detect() -> Self {
        Self::from_env_or_platform(std::env::var("DTB_KE_SKIN").ok().as_deref())
    }

    pub(crate) fn from_env_or_platform(forced: Option<&str>) -> Self {
        if let Some(value) = forced {
            match value.trim().to_ascii_lowercase().as_str() {
                "mac" | "macos" => return Self::MacLiquidGlass,
                "win" | "windows" => return Self::WinUi3,
                "linux" => return Self::LinuxNeutral,
                "" => {}
                other => {
                    eprintln!("DTB_KE_SKIN: unknown value {other:?}, using the platform default");
                }
            }
        }
        Self::platform_default()
    }

    pub(crate) const fn platform_default() -> Self {
        #[cfg(target_os = "macos")]
        {
            Self::MacLiquidGlass
        }
        #[cfg(target_os = "windows")]
        {
            Self::WinUi3
        }
        #[cfg(not(any(target_os = "macos", target_os = "windows")))]
        {
            Self::LinuxNeutral
        }
    }

    fn slug(self) -> &'static str {
        match self {
            Self::MacLiquidGlass => "macos",
            Self::WinUi3 => "windows",
            Self::LinuxNeutral => "linux",
        }
    }

    /// The skin TOML compiled into the binary.
    pub(crate) fn embedded_toml(self) -> &'static str {
        match self {
            Self::MacLiquidGlass => include_str!("../../themes/skins/macos.toml"),
            Self::WinUi3 => include_str!("../../themes/skins/windows.toml"),
            Self::LinuxNeutral => include_str!("../../themes/skins/linux.toml"),
        }
    }

    /// The on-disk path to this skin's TOML in the source tree. Debug builds
    /// prefer this so the theme can be iterated without a rebuild.
    #[cfg(debug_assertions)]
    pub(crate) fn disk_path(self) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("themes/skins")
            .join(format!("{}.toml", self.slug()))
    }
}

/// Window background material. Interpreted by `skin/window.rs` in a later phase;
/// carried here so it can be authored per skin.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowMaterial {
    Blurred,
    Mica,
    MicaAlt,
    Opaque,
}

/// Server-side (OS frame) vs client-side (we draw it) window decorations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum WindowDecorations {
    Server,
    Client,
}

/// Where the application menu bar is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MenuStyle {
    /// The OS draws it (macOS system menu bar). gpui `cx.set_menus`.
    Native,
    /// We draw a full menu strip in the chrome (Windows).
    InApp,
    /// We draw a single "☰" button opening one popover with every menu
    /// (Linux / GNOME-style).
    Hamburger,
}

/// How a focused control announces focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FocusRing {
    /// The whole outline switches to the accent colour (macOS, Linux).
    Outline,
    /// A 2px accent bar inside the bottom edge; the rest of the border is
    /// unchanged (WinUI 3).
    Underline,
}

/// How a selected sidebar row is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SelectionStyle {
    GlassTint,
    WinuiPill,
    SolidSubtle,
}

/// The outline shape of labelled controls (buttons, segmented tracks).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ControlShape {
    /// Corners follow [`SkinMetrics::radius_control`].
    #[default]
    Rounded,
    /// Fully rounded ends — Liquid Glass's capsule controls.
    Capsule,
}

/// Elevation character.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShadowKind {
    Soft,
    Tight,
    Flat,
}

/// Structural metrics for one platform skin, deserialised from a skin TOML.
///
/// `*_px` accessors wrap the raw `f32` in `gpui_kit::Pixels`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct SkinMetrics {
    pub material: WindowMaterial,
    /// Alpha applied to the chrome (title bar / sidebar) fill when [`Self::material`]
    /// is translucent. `1.0` for opaque skins.
    pub material_opacity: f32,
    /// Native glass surfaces (macOS 26+): how strongly the palette's `chrome`
    /// colour tints the glass itself, `0.0..=1.0`. `0.0` = the untinted system
    /// glass.
    #[serde(default)]
    pub glass_tint_opacity: f32,
    /// Native glass surfaces: alpha of a `chrome`-coloured fill placed
    /// *behind* the glass, so it samples something body-ful instead of the bare
    /// desktop (Xcode's sidebar samples the window's opaque content). `0.0` =
    /// none.
    #[serde(default)]
    pub glass_backing_opacity: f32,
    /// Dark-appearance overrides for the two glass strengths above. Glass
    /// lifts the brightness of whatever is behind it, so dark mode usually
    /// needs a stronger fill to sit level with the rest of the dark UI. Unset
    /// = same as light.
    #[serde(default)]
    pub glass_tint_opacity_dark: Option<f32>,
    #[serde(default)]
    pub glass_backing_opacity_dark: Option<f32>,
    /// Alpha for a plain-blur backdrop specifically — macOS's native blur, or
    /// Windows' Acrylic-blur-behind fallback when Mica isn't available (see
    /// `material::window_background`'s doc comment). A flat blur reads
    /// thinner than Mica at the same alpha, so this can run a little higher
    /// than [`Self::material_opacity`]. Omit to just reuse `material_opacity`
    /// unchanged — this is why every skin doesn't have to set it.
    #[serde(default)]
    pub material_opacity_blurred: Option<f32>,
    pub window_decorations: WindowDecorations,
    pub menu_bar: MenuStyle,
    pub title_bar_height: f32,
    pub radius: f32,
    pub radius_lg: f32,
    pub radius_control: f32,
    /// Outline shape of buttons and segmented controls. Unset = `rounded`.
    #[serde(default)]
    pub control_shape: ControlShape,
    pub focus_ring: FocusRing,
    pub selection_style: SelectionStyle,
    pub shadow: ShadowKind,
    /// Empty string means "gpui system UI default".
    pub font_family: String,
    /// Monospace family for data-ish labels (meeting times, section headers).
    /// Empty string means "the platform generic monospace".
    pub mono_font_family: String,
    pub motion_scale: f32,
}

impl SkinMetrics {
    pub fn parse(toml_src: &str) -> Result<Self, ThemeError> {
        Ok(toml::from_str(toml_src)?)
    }

    pub fn radius_px(&self) -> Pixels {
        px(self.radius)
    }
    pub fn radius_lg_px(&self) -> Pixels {
        px(self.radius_lg)
    }
    pub fn radius_control_px(&self) -> Pixels {
        px(self.radius_control)
    }
    /// Corner radius for a pill nested inside a `radius_control`-rounded,
    /// `p(px(2.))`-padded container (segmented-style pickers), so the inner
    /// and outer corners stay concentric instead of the inner one reading as
    /// a plain rectangle.
    pub fn radius_control_inset_px(&self) -> Pixels {
        px((self.radius_control - 2.0).max(2.0))
    }
    /// Corner radius for a labelled control of `height`: half the height for a
    /// capsule skin, else [`Self::radius_control_px`].
    pub fn control_radius_px(&self, height: Pixels) -> Pixels {
        match self.control_shape {
            ControlShape::Capsule => height / 2.0,
            ControlShape::Rounded => self.radius_control_px(),
        }
    }

    pub fn title_bar_height_px(&self) -> Pixels {
        px(self.title_bar_height)
    }

    /// The chrome-fill alpha to use for a translucent window right now —
    /// [`Self::material_opacity_blurred`] for a plain blur backdrop, falling
    /// back to [`Self::material_opacity`] when unset; [`Self::material_opacity`]
    /// unchanged for a real Mica/MicaAlt backdrop.
    pub fn material_opacity_for(&self, blurred: bool) -> f32 {
        if blurred {
            self.material_opacity_blurred
                .unwrap_or(self.material_opacity)
        } else {
            self.material_opacity
        }
    }

    /// The font family to use, or `None` for gpui's system UI default.
    pub fn font_family(&self) -> Option<SharedString> {
        non_empty(&self.font_family)
    }

    /// The monospace family, falling back to the generic `"monospace"`.
    pub fn mono_font_family(&self) -> SharedString {
        non_empty(&self.mono_font_family).unwrap_or_else(|| SharedString::from("monospace"))
    }

    /// An animation duration scaled by this skin's `motion_scale`.
    pub fn motion(&self, base: std::time::Duration) -> std::time::Duration {
        base.mul_f32(self.motion_scale.max(0.0))
    }
}

fn non_empty(value: &str) -> Option<SharedString> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| SharedString::from(trimmed.to_owned()))
}
