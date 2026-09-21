//! Window material — the restrained "Liquid Glass" look on macOS, Mica (or a
//! real fallback) on Windows, plain opaque on Linux.
//!
//! When the active skin asks for a translucent window ([`WindowMaterial`]
//! other than `Opaque`) and the running OS can actually render it, the chrome
//! (title bar, sidebar) is painted at reduced opacity so the real backdrop
//! shows through. The **content** area always stays fully opaque — judging
//! tables must never sit on a moving backdrop. On Windows, whether it's
//! *actually* translucent depends on the OS build — [`effective`] resolves
//! that for real via [`crate::skin::window::windows_backdrop_support`] rather
//! than trusting the skin's nominal material, so an older build falls all the
//! way back to a plain opaque window instead of a half-drawn, hard-to-read
//! one. The user's `Settings::reduce_transparency` overrides all of this to
//! `Opaque` outright, same idea as the OS-level "reduce transparency"
//! accessibility toggles this mirrors.
//!
//! Colours are still token-derived: the translucent fills are the palette's
//! `chrome` role, at an alpha the active skin authors
//! ([`crate::theme::SkinMetrics::material_opacity_for`]).

use gpui::{App, Hsla, WindowBackgroundAppearance};

use crate::settings::Settings;
use crate::skin::window::WindowsBackdropSupport;
use crate::theme::{ActiveTheme, Theme, WindowMaterial};

/// What actually gets requested from the OS / painted as the chrome's
/// alpha-blend target — the skin's nominal [`WindowMaterial`], resolved
/// against runtime facts (the real Windows OS build, the user's "reduce
/// transparency" setting) that can step it down to something plainer.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Effective {
    Opaque,
    /// A real blur — Windows' Acrylic blur-behind fallback when Mica isn't
    /// available.
    Blurred,
    /// macOS: a transparent gpui window over our own stock
    /// `NSVisualEffectView` ([`crate::skin::backdrop`]). The system material
    /// carries its own tint, so our chrome paints no fill of its own.
    Native,
    Mica,
    MicaAlt,
}

/// Resolve the skin's nominal material down to what's actually going to be
/// drawn right now. See the module doc comment and [`window_background`]'s
/// for the full fallback chain this implements.
fn effective(theme: &Theme, cx: &App) -> Effective {
    if Settings::global(cx).reduce_transparency {
        return Effective::Opaque;
    }
    match theme.skin.material {
        WindowMaterial::Opaque => Effective::Opaque,
        WindowMaterial::Blurred if crate::skin::backdrop::available() => Effective::Native,
        WindowMaterial::Blurred => Effective::Blurred,
        WindowMaterial::Mica => match crate::skin::window::windows_backdrop_support() {
            WindowsBackdropSupport::Mica => Effective::Mica,
            WindowsBackdropSupport::Acrylic => Effective::Blurred,
            WindowsBackdropSupport::None => Effective::Opaque,
        },
        WindowMaterial::MicaAlt => match crate::skin::window::windows_backdrop_support() {
            WindowsBackdropSupport::Mica => Effective::MicaAlt,
            WindowsBackdropSupport::Acrylic => Effective::Blurred,
            WindowsBackdropSupport::None => Effective::Opaque,
        },
    }
}

/// Whether the active skin's window material is actually going to render as
/// translucent on this machine right now — not just whether it's nominally
/// non-`Opaque`. See [`effective`].
pub fn is_translucent(theme: &Theme, cx: &App) -> bool {
    effective(theme, cx) != Effective::Opaque
}

/// The background mode to request when opening (or re-requesting for an
/// already-open) window.
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
/// way down to `Opaque` on anything older that supports neither, or when the
/// user has `Settings::reduce_transparency` on. [`is_translucent`] mirrors
/// the same resolution so the chrome never paints translucent over a
/// backdrop that was never actually drawn.
pub fn window_background(theme: &Theme, cx: &App) -> WindowBackgroundAppearance {
    match effective(theme, cx) {
        Effective::Opaque => WindowBackgroundAppearance::Opaque,
        Effective::Blurred => WindowBackgroundAppearance::Blurred,
        Effective::Native => WindowBackgroundAppearance::Transparent,
        Effective::Mica => WindowBackgroundAppearance::MicaBackdrop,
        Effective::MicaAlt => WindowBackgroundAppearance::MicaAltBackdrop,
    }
}

/// Re-requests [`window_background`] on every open window and repaints them —
/// call after `Settings::reduce_transparency` (or anything else `effective`
/// depends on) changes, so already-open windows pick it up immediately
/// instead of only the next one opened. `cx.refresh_windows()` alone would
/// redraw our own chrome at the new alpha but never touch the OS-level
/// backdrop itself — that needs this explicit per-window call.
pub fn apply_background_to_all_windows(cx: &mut App) {
    for handle in cx.windows() {
        let is_main = crate::app::is_main_window(handle);
        let _ = handle.update(cx, |_, window, cx| {
            let appearance = window_background(cx.theme(), cx);
            window.set_background_appearance(appearance);
            if is_main {
                sync_native_backdrop(window, cx);
            }
        });
    }
    cx.refresh_windows();
}

/// Installs or removes the main window's native backdrop to match
/// [`effective`] — a no-op off macOS, where nothing resolves to `Native`.
pub fn sync_native_backdrop(window: &gpui::Window, cx: &App) {
    if effective(cx.theme(), cx) == Effective::Native {
        crate::skin::backdrop::install(window, cx.theme().mode);
    } else {
        crate::skin::backdrop::remove();
    }
}

fn with_alpha(color: Hsla, alpha: f32) -> Hsla {
    Hsla { a: alpha, ..color }
}

/// Fill for the window root. Transparent under a translucent skin (so the blur
/// is visible), the opaque background otherwise.
pub fn root_fill(theme: &Theme, cx: &App) -> Hsla {
    if is_translucent(theme, cx) {
        gpui::transparent_black()
    } else {
        theme.color.background
    }
}

/// Fill for the title bar / sidebar chrome — translucent (at the skin's
/// [`crate::theme::SkinMetrics::material_opacity_for`]) when [`effective`]
/// resolves to a real backdrop, otherwise the plain opaque chrome colour.
fn chrome_like_fill(theme: &Theme, cx: &App) -> Hsla {
    match effective(theme, cx) {
        Effective::Opaque => theme.color.chrome,
        Effective::Blurred => with_alpha(theme.color.chrome, theme.skin.material_opacity_for(true)),
        Effective::Native => gpui::transparent_black(),
        Effective::Mica | Effective::MicaAlt => {
            with_alpha(theme.color.chrome, theme.skin.material_opacity_for(false))
        }
    }
}

/// Fill for the title bar / toolbar band.
pub fn chrome_fill(theme: &Theme, cx: &App) -> Hsla {
    chrome_like_fill(theme, cx)
}

/// Fill for the toolbar band that sits above the content pane. Same as
/// [`chrome_fill`] except under the native backdrop, where the band is part of
/// the content column and paints the opaque content colour — the system
/// material stays visible only behind the sidebar.
pub fn toolbar_fill(theme: &Theme, cx: &App) -> Hsla {
    match effective(theme, cx) {
        Effective::Native => content_fill(theme),
        _ => chrome_like_fill(theme, cx),
    }
}

/// Fill for the navigation sidebar.
pub fn sidebar_fill(theme: &Theme, cx: &App) -> Hsla {
    chrome_like_fill(theme, cx)
}

/// Fill for the content pane — always fully opaque.
pub fn content_fill(theme: &Theme) -> Hsla {
    theme.color.background
}
