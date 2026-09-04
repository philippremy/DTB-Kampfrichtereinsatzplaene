//! Window material — the restrained "Liquid Glass" look on macOS, plain opaque
//! everywhere else (for now).
//!
//! When the active skin asks for a translucent window ([`WindowMaterial`] other
//! than `Opaque`), the window background is [`WindowBackgroundAppearance::Blurred`]
//! and the chrome (title bar, sidebar) is painted at reduced opacity so the
//! blur shows through. The **content** area always stays fully opaque — judging
//! tables must never sit on a moving backdrop.
//!
//! Colours are still token-derived: the translucent fills are the palette's
//! `chrome` / `surface` roles with the skin's `material_opacity` alpha.

use gpui::{Hsla, WindowBackgroundAppearance};

use crate::theme::{Theme, WindowMaterial};

/// Whether the active skin wants a translucent (blurred) window.
pub fn is_translucent(theme: &Theme) -> bool {
    !matches!(theme.skin.material, WindowMaterial::Opaque)
}

/// The background mode to request when opening a window.
pub fn window_background(theme: &Theme) -> WindowBackgroundAppearance {
    match theme.skin.material {
        WindowMaterial::Blurred => WindowBackgroundAppearance::Blurred,
        // Phase 7 maps these to the real Mica backdrops; a blur is the closest
        // approximation until then.
        WindowMaterial::Mica => WindowBackgroundAppearance::MicaBackdrop,
        WindowMaterial::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
        WindowMaterial::Opaque => WindowBackgroundAppearance::Opaque,
    }
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// Fill for the window root. Transparent under a translucent skin (so the blur
/// is visible), the opaque background otherwise.
pub fn root_fill(theme: &Theme) -> Hsla {
    if is_translucent(theme) {
        gpui::transparent_black()
    } else {
        theme.color.background
    }
}

/// Fill for the title bar / toolbar band.
pub fn chrome_fill(theme: &Theme) -> Hsla {
    if is_translucent(theme) {
        with_alpha(theme.color.chrome, theme.skin.material_opacity)
    } else {
        theme.color.chrome
    }
}

/// Fill for the navigation sidebar.
pub fn sidebar_fill(theme: &Theme) -> Hsla {
    if is_translucent(theme) {
        with_alpha(theme.color.chrome, theme.skin.material_opacity)
    } else {
        theme.color.chrome
    }
}

/// Fill for the content pane — always fully opaque.
pub fn content_fill(theme: &Theme) -> Hsla {
    theme.color.background
}
