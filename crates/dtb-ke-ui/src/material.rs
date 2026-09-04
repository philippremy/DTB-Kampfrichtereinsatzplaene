//! Window material — the restrained "Liquid Glass" look on macOS, Mica (or a
//! real fallback) on Windows, plain opaque on Linux.
//!
//! When the active skin asks for a translucent window ([`WindowMaterial`]
//! other than `Opaque`) and the running OS can actually render it, the chrome
//! (title bar, sidebar) is painted at reduced opacity so the real backdrop
//! shows through. The **content** area always stays fully opaque — judging
//! tables must never sit on a moving backdrop. On Windows, whether it's
//! *actually* translucent depends on the OS build —
//! [`is_translucent`]/[`window_background`] both resolve that for real via
//! [`crate::skin::window::windows_backdrop_support`] rather than trusting the
//! skin's nominal material, so an older build falls all the way back to a
//! plain opaque window instead of a half-drawn, hard-to-read one.
//!
//! Colours are still token-derived: the translucent fills are the palette's
//! `chrome` / `surface` roles with the skin's `material_opacity` alpha.

use gpui::{Hsla, WindowBackgroundAppearance};

use crate::skin::window::WindowsBackdropSupport;
use crate::theme::{Theme, WindowMaterial};

/// Whether the active skin's window material is actually going to render as
/// translucent on this machine — not just whether it's nominally non-`Opaque`.
///
/// `Mica`/`MicaAlt` only have a visible effect on Windows 11 22H2+; see
/// [`window_background`]'s doc comment for the full fallback chain. If the
/// running build can't draw anything behind the window, painting our own
/// chrome (title bar, sidebar) at reduced opacity anyway is exactly the
/// "merely opacity-reduced, hard to read" bug this exists to avoid — so this
/// checks the same *effective*, already-stepped-down material that
/// [`window_background`] actually requests, not the skin's nominal one.
pub fn is_translucent(theme: &Theme) -> bool {
    match theme.skin.material {
        WindowMaterial::Opaque => false,
        WindowMaterial::Blurred => true,
        WindowMaterial::Mica | WindowMaterial::MicaAlt => {
            crate::skin::window::windows_backdrop_support() != WindowsBackdropSupport::None
        }
    }
}

/// The background mode to request when opening a window.
///
/// `Mica`/`MicaAlt` only ever have a visible effect on Windows 11 22H2+
/// (`DWMWA_SYSTEMBACKDROP_TYPE`, build 22621) — asking for either on an
/// older build silently no-ops in gpui's own Windows backend, with no
/// fallback of its own. That left the window exactly as translucent as our
/// *own* chrome painting assumed a real backdrop would be, with nothing
/// actually drawn behind it: a merely opacity-reduced, hard-to-read window
/// on Windows 10 (and pre-22H2 Windows 11).
/// [`crate::skin::window::windows_backdrop_support`] detects the real
/// running build and steps this down instead: `Blurred` on Windows 10
/// 1809+ — which gpui sends on Windows via the older, undocumented
/// `SetWindowCompositionAttribute` API's `ACCENT_ENABLE_ACRYLICBLURBEHIND`
/// state, i.e. a real Acrylic-style blur, not a plain translucency — all the
/// way down to `Opaque` on anything older that supports neither.
/// [`is_translucent`] mirrors the same fallback so the chrome never paints
/// translucent over a backdrop that was never actually drawn.
pub fn window_background(theme: &Theme) -> WindowBackgroundAppearance {
    match theme.skin.material {
        WindowMaterial::Blurred => WindowBackgroundAppearance::Blurred,
        WindowMaterial::Mica => match crate::skin::window::windows_backdrop_support() {
            WindowsBackdropSupport::Mica => WindowBackgroundAppearance::MicaBackdrop,
            WindowsBackdropSupport::Acrylic => WindowBackgroundAppearance::Blurred,
            WindowsBackdropSupport::None => WindowBackgroundAppearance::Opaque,
        },
        WindowMaterial::MicaAlt => match crate::skin::window::windows_backdrop_support() {
            WindowsBackdropSupport::Mica => WindowBackgroundAppearance::MicaAltBackdrop,
            WindowsBackdropSupport::Acrylic => WindowBackgroundAppearance::Blurred,
            WindowsBackdropSupport::None => WindowBackgroundAppearance::Opaque,
        },
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
