//! The application's own theme system.
//!
//! Two axes, both authored as TOML compiled into the binary (`themes/`):
//!
//! * a **skin** ([`SkinMetrics`]) — structure: radii, material, decorations,
//!   focus/selection style, shadow, font, motion. One file per platform.
//! * a **palette** ([`Palette`]) — a `[light]` and `[dark]` block of semantic
//!   colour roles.
//!
//! [`Theme::install`] resolves the pair for the current [`Skin`] and
//! [`Appearance`], installs it as a gpui global (read through [`ActiveTheme`]),
//! and mirrors the colours into `gpui_base::Theme` so the behaviour-layer
//! primitives from `gpui-base` match.
//!
//! Debug builds re-read the TOML from the source tree so the design can be
//! iterated without a rebuild (call [`Theme::reload`]); release builds use only
//! the embedded copies.

mod palette;
mod skin;

pub use palette::{Palette, PaletteColors};
// Several of these are consumed only by later phases (skin/window.rs, the
// component layer) and the test module.
#[allow(unused_imports)]
pub use skin::{
    FocusRing, MenuStyle, SelectionStyle, ShadowKind, Skin, SkinMetrics, WindowDecorations,
    WindowMaterial,
};

use std::borrow::Cow;

use gpui::{App, Global, WindowAppearance};
use serde::{Deserialize, Serialize};

const PALETTE_DTB_EMBEDDED: &str = include_str!("../themes/palettes/dtb.toml");

#[derive(Debug, thiserror::Error)]
pub enum ThemeError {
    #[error("theme TOML is invalid: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid colour {0:?} — expected #rgb, #rgba, #rrggbb or #rrggbbaa")]
    Color(String),
}

/// The user's appearance preference (persisted in `Settings.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThemeMode {
    /// Follow the OS.
    #[default]
    System,
    Light,
    Dark,
}

/// A resolved appearance — what `System` becomes once the OS is consulted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    Light,
    Dark,
}

impl ThemeMode {
    /// Resolve to a concrete [`Appearance`] given the OS's current appearance.
    pub fn resolve(self, os: Appearance) -> Appearance {
        match self {
            ThemeMode::System => os,
            ThemeMode::Light => Appearance::Light,
            ThemeMode::Dark => Appearance::Dark,
        }
    }

    /// The next mode in the System → Light → Dark → System cycle.
    pub fn cycled(self) -> Self {
        match self {
            ThemeMode::System => ThemeMode::Light,
            ThemeMode::Light => ThemeMode::Dark,
            ThemeMode::Dark => ThemeMode::System,
        }
    }

    /// A translated label for the toggle.
    pub fn label(self, locale: &crate::i18n::Locale) -> gpui::SharedString {
        use crate::i18n::ActiveLocale;
        let key = match self {
            ThemeMode::System => "settings.general.theme-system",
            ThemeMode::Light => "settings.general.theme-light",
            ThemeMode::Dark => "settings.general.theme-dark",
        };
        locale.t(key)
    }
}

impl From<WindowAppearance> for Appearance {
    fn from(appearance: WindowAppearance) -> Self {
        match appearance {
            WindowAppearance::Light | WindowAppearance::VibrantLight => Appearance::Light,
            WindowAppearance::Dark | WindowAppearance::VibrantDark => Appearance::Dark,
        }
    }
}

/// The active theme: skin metrics + palette colours for the current appearance.
#[derive(Debug, Clone)]
pub struct Theme {
    /// The user's preference.
    pub mode: ThemeMode,
    /// What the OS currently reports.
    pub os_appearance: Appearance,
    /// The effective appearance = `mode.resolve(os_appearance)`.
    pub appearance: Appearance,
    pub skin: SkinMetrics,
    pub color: PaletteColors,
}

impl Global for Theme {}

/// Read the active [`Theme`] from any app context.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    #[inline]
    fn theme(&self) -> &Theme {
        self.global::<Theme>()
    }
}

impl Theme {
    /// Resolve and install the theme as the global. Call once at startup with
    /// the persisted [`ThemeMode`] and the OS's current [`Appearance`].
    pub fn install(mode: ThemeMode, os_appearance: Appearance, cx: &mut App) {
        Self::apply(mode, os_appearance, cx);
    }

    /// The user picked a different mode (persists it).
    pub fn set_mode(mode: ThemeMode, cx: &mut App) {
        log::info!("theme mode changed to {mode:?}");
        let os = cx.global::<Theme>().os_appearance;
        Self::apply(mode, os, cx);
        crate::settings::Settings::update(cx, |settings| settings.theme_mode = mode);
    }

    /// The OS switched light/dark (from `observe_window_appearance`).
    pub fn set_os_appearance(os_appearance: Appearance, cx: &mut App) {
        let (mode, current) = {
            let t = cx.global::<Theme>();
            (t.mode, t.os_appearance)
        };
        if current != os_appearance {
            log::debug!("OS appearance switched to {os_appearance:?}");
            Self::apply(mode, os_appearance, cx);
        }
    }

    pub fn mode(cx: &App) -> ThemeMode {
        cx.global::<Theme>().mode
    }

    /// Re-resolve with the current mode / OS appearance and replace the global.
    /// In debug builds this picks up edits to the TOML files.
    pub fn reload(cx: &mut App) {
        let (mode, os) = {
            let t = cx.global::<Theme>();
            (t.mode, t.os_appearance)
        };
        Self::apply(mode, os, cx);
    }

    fn apply(mode: ThemeMode, os_appearance: Appearance, cx: &mut App) {
        let use_system_accent = crate::settings::Settings::global(cx).use_system_accent_color;
        let theme = Self::resolve_from(mode, os_appearance, Source::Preferred, use_system_accent)
            .unwrap_or_else(|err| {
                // Disk overrides (debug) can be broken by an edit; the embedded
                // assets are covered by tests and must not fail.
                log::warn!("theme: {err}; falling back to the embedded assets");
                Self::resolve_from(mode, os_appearance, Source::Embedded, use_system_accent)
                    .expect("embedded theme assets must be valid")
            });
        log::debug!(
            "theme applied — skin {:?}, mode {:?}, appearance {:?}",
            Skin::detect(),
            mode,
            theme.appearance
        );
        theme.bridge_to_base(cx);
        cx.set_global(theme);
        cx.refresh_windows();
    }

    fn resolve_from(
        mode: ThemeMode,
        os_appearance: Appearance,
        source: Source,
        use_system_accent: bool,
    ) -> Result<Self, ThemeError> {
        let appearance = mode.resolve(os_appearance);
        let skin_id = Skin::detect();
        let skin = SkinMetrics::parse(&source.skin_toml(skin_id))?;
        let palette = Palette::parse(&source.palette_toml())?;
        let mut color = palette.colors(appearance)?;
        if use_system_accent && let Some(accent) = crate::skin::accent::system_accent(appearance) {
            color.apply_system_accent(accent);
        }
        Ok(Self {
            mode,
            os_appearance,
            appearance,
            skin,
            color,
        })
    }

    /// Mirror the resolved colours and radii into `gpui_base::Theme` so the
    /// `gpui-base` behaviour primitives (Input, Scrollbar, …) match our look.
    fn bridge_to_base(&self, cx: &mut App) {
        use gpui_base::ThemeAppearance;

        let c = &self.color;
        let base = gpui_base::Theme::global_mut(cx);

        base.appearance = match self.appearance {
            Appearance::Light => ThemeAppearance::Light,
            Appearance::Dark => ThemeAppearance::Dark,
        };

        // Assign field-by-field rather than a struct literal: `gpui-base` is a
        // fast-moving git dependency and `ColorTokens` gains fields over time.
        let colors = &mut base.tokens.colors;
        colors.background = c.background;
        colors.foreground = c.foreground;
        colors.surface = c.surface;
        colors.surface_foreground = c.surface_foreground;
        colors.primary = c.primary;
        colors.primary_foreground = c.primary_foreground;
        colors.secondary = c.surface;
        colors.secondary_foreground = c.foreground;
        colors.muted = c.chrome;
        colors.muted_foreground = c.muted_foreground;
        colors.accent = c.accent_soft;
        colors.accent_foreground = c.primary;
        colors.destructive = c.critical;
        colors.destructive_foreground = c.destructive_foreground;
        colors.border = c.border;
        colors.input = c.border;
        colors.ring = c.ring;
        colors.selection = gpui::Hsla {
            a: 0.24,
            ..c.primary
        };

        base.tokens.radius.sm = self.skin.radius_control_px();
        base.tokens.radius.md = self.skin.radius_px();
        base.tokens.radius.lg = self.skin.radius_lg_px();

        // Scrollbar thumbs default to square (`ScrollbarThumbStyle`'s own
        // built-in fallback is `Pixels::ZERO`) unless a theme opts in. Every
        // skin's control radius exceeds half the thumb's width, so this
        // resolves to a full pill on all three (the library's own
        // `clamp_thumb_radius` caps an oversized request to that) — the
        // modern native look on macOS, Windows 11, and GNOME alike.
        let thumb_radius = self.skin.radius_control_px();
        base.scrollbar = gpui_base::ScrollbarTheme::new().with_styles(
            gpui_base::ScrollbarStyles::default()
                .thumb(|t| t.radius(thumb_radius))
                .thumb_hover(|t| t.radius(thumb_radius))
                .thumb_active(|t| t.radius(thumb_radius)),
        );
    }
}

/// Where a resolve reads its TOML from.
#[derive(Clone, Copy)]
enum Source {
    /// Disk in debug builds, embedded in release.
    Preferred,
    /// Always the compiled-in copy.
    Embedded,
}

impl Source {
    fn skin_toml(self, skin: Skin) -> Cow<'static, str> {
        #[cfg(debug_assertions)]
        if matches!(self, Source::Preferred) {
            let path = skin.disk_path();
            match std::fs::read_to_string(&path) {
                Ok(src) => return Cow::Owned(src),
                Err(err) => {
                    log::debug!(
                        "theme: could not read {}: {err}; using embedded",
                        path.display()
                    );
                }
            }
        }
        let _ = self;
        Cow::Borrowed(skin.embedded_toml())
    }

    fn palette_toml(self) -> Cow<'static, str> {
        #[cfg(debug_assertions)]
        if matches!(self, Source::Preferred) {
            let path =
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("themes/palettes/dtb.toml");
            match std::fs::read_to_string(&path) {
                Ok(src) => return Cow::Owned(src),
                Err(err) => {
                    log::debug!(
                        "theme: could not read {}: {err}; using embedded",
                        path.display()
                    );
                }
            }
        }
        let _ = self;
        Cow::Borrowed(PALETTE_DTB_EMBEDDED)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hex_shapes_and_errors() {
        assert!(palette::hex("#000000").is_ok());
        assert_eq!(
            palette::hex("#ff0000").unwrap(),
            palette::hex("f00").unwrap()
        );
        assert_eq!(
            palette::hex("#ffffffff").unwrap(),
            palette::hex("#ffffff").unwrap()
        );

        let translucent = palette::hex("#00000080").unwrap();
        assert!((translucent.a - 0.5).abs() < 0.01);

        assert!(palette::hex("#12").is_err());
        assert!(palette::hex("nothex").is_err());
        assert!(palette::hex("#gggggg").is_err());
    }

    #[test]
    fn embedded_skins_parse_with_expected_materials() {
        let mac = SkinMetrics::parse(Skin::MacLiquidGlass.embedded_toml()).unwrap();
        assert_eq!(mac.material, WindowMaterial::Blurred);
        assert_eq!(mac.window_decorations, WindowDecorations::Server);
        assert_eq!(mac.selection_style, SelectionStyle::GlassTint);
        assert_eq!(mac.focus_ring, FocusRing::Outline);
        assert_eq!(mac.menu_bar, MenuStyle::Native);

        let win = SkinMetrics::parse(Skin::WinUi3.embedded_toml()).unwrap();
        assert_eq!(win.material, WindowMaterial::Mica);
        assert_eq!(win.focus_ring, FocusRing::Underline);
        assert_eq!(win.menu_bar, MenuStyle::InApp);

        let linux = SkinMetrics::parse(Skin::LinuxNeutral.embedded_toml()).unwrap();
        assert_eq!(linux.material, WindowMaterial::Opaque);
        assert_eq!(linux.window_decorations, WindowDecorations::Client);
        assert_eq!(linux.focus_ring, FocusRing::Outline);
        assert_eq!(linux.menu_bar, MenuStyle::Hamburger);
        assert!(linux.font_family().is_none());
    }

    #[test]
    fn embedded_palette_resolves_both_appearances() {
        let palette = Palette::parse(PALETTE_DTB_EMBEDDED).unwrap();
        assert_eq!(palette.name, "DTB");
        let light = palette.colors(Appearance::Light).unwrap();
        let dark = palette.colors(Appearance::Dark).unwrap();
        // Light ground is lighter than dark ground.
        assert!(light.background.l > dark.background.l);
        // The emblem plate stays light even in the dark theme.
        assert!(dark.emblem_plate.l > 0.7);
        assert!(light.emblem_plate.l > 0.9);
    }

    #[test]
    fn skin_env_override() {
        assert_eq!(Skin::from_env_or_platform(Some("win")), Skin::WinUi3);
        assert_eq!(
            Skin::from_env_or_platform(Some("  MacOS ")),
            Skin::MacLiquidGlass
        );
        assert_eq!(
            Skin::from_env_or_platform(Some("linux")),
            Skin::LinuxNeutral
        );
        assert_eq!(
            Skin::from_env_or_platform(Some("bogus")),
            Skin::platform_default()
        );
        assert_eq!(Skin::from_env_or_platform(None), Skin::platform_default());
    }

    #[test]
    fn theme_mode_resolves() {
        assert_eq!(
            ThemeMode::System.resolve(Appearance::Dark),
            Appearance::Dark
        );
        assert_eq!(
            ThemeMode::Light.resolve(Appearance::Dark),
            Appearance::Light
        );
    }

    #[test]
    fn theme_mode_cycles_and_round_trips_toml() {
        assert_eq!(ThemeMode::System.cycled(), ThemeMode::Light);
        assert_eq!(ThemeMode::Light.cycled(), ThemeMode::Dark);
        assert_eq!(ThemeMode::Dark.cycled(), ThemeMode::System);

        for mode in [ThemeMode::System, ThemeMode::Light, ThemeMode::Dark] {
            let wrapped = toml::to_string(&Wrap { theme_mode: mode }).unwrap();
            let back: Wrap = toml::from_str(&wrapped).unwrap();
            assert_eq!(back.theme_mode, mode);
        }
    }

    #[derive(serde::Serialize, serde::Deserialize)]
    struct Wrap {
        theme_mode: ThemeMode,
    }

    #[test]
    fn appearance_from_window_appearance() {
        use gpui::WindowAppearance;
        assert_eq!(
            Appearance::from(WindowAppearance::VibrantLight),
            Appearance::Light
        );
        assert_eq!(Appearance::from(WindowAppearance::Dark), Appearance::Dark);
    }
}
