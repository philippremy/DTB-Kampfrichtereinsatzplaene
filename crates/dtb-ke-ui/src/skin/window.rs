//! Window construction, derived from the active skin.

use gpui::{App, Bounds, Size, TitlebarOptions, WindowBounds, WindowOptions, point, px};

use crate::material;
use crate::skin::decorations;
use crate::theme::{ActiveTheme, WindowDecorations};

/// Options for the app's main window.
///
/// The window background follows the active skin's material (`Blurred` on
/// macOS; Mica on Windows 11 22H2+, stepping down to a real Acrylic-style
/// blur on older Windows 10/11 and to opaque below that — see
/// [`material::window_background`]'s doc comment; opaque on Linux); the
/// Linux skin also requests client-side decorations so the app draws its own
/// frame (see [`crate::skin::decorations`]).
pub fn main_window_options(cx: &mut App) -> WindowOptions {
    let theme = cx.theme();
    let window_background = material::window_background(theme, cx);
    let client_decorations = theme.skin.window_decorations == WindowDecorations::Client;

    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(1100.), px(760.)),
            cx,
        ))),
        window_min_size: Some(Size::new(px(720.), px(480.))),
        titlebar: Some(TitlebarOptions {
            title: Some("DTB Kampfrichtereinsatzpläne".into()),
            // macOS draws into a transparent OS title bar; on Windows our context
            // band sits below the OS title bar; Linux is frameless (CSD).
            appears_transparent: cfg!(target_os = "macos") || client_decorations,
            traffic_light_position: Some(point(px(14.), px(19.))),
        }),
        window_background,
        window_decorations: Some(decorations::requested(cx.theme())),
        ..Default::default()
    }
}

/// Whether a secondary/utility window (About, Feedback, Preview, Logs,
/// Settings) should ask for a transparent title bar.
///
/// True only on macOS. There, a transparent title bar still keeps the
/// native traffic lights and just blends the bar with our content
/// underneath. On Windows the same flag means something completely
/// different to gpui: `appears_transparent: true` makes it hide the
/// OS-drawn caption bar — min/max/close included — entirely, on the
/// assumption the app will draw its own controls via `WindowControlArea`
/// (which is exactly what `AppShell`'s main window does — see
/// `app.rs::window_controls`). None of these utility windows draw anything
/// of the kind, so requesting it there left them with no way to close on
/// Windows. Linux ignores this flag outright — its native-vs-client chrome
/// is governed entirely by `WindowOptions::window_decorations` — so this
/// only actually changes anything for macOS vs. Windows.
pub fn secondary_window_appears_transparent() -> bool {
    cfg!(target_os = "macos")
}

/// Which translucent Windows backdrop the *running* OS build actually
/// supports — used by [`crate::material::window_background`] to step the
/// Windows skin's requested `Mica`/`MicaAlt` material down to whatever the
/// machine can really render, instead of silently requesting something that
/// draws nothing.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum WindowsBackdropSupport {
    /// Windows 11 22H2+ (build 22621) — the real Mica/MicaAlt system
    /// backdrop via `DWMWA_SYSTEMBACKDROP_TYPE`.
    Mica,
    /// Windows 10 1809+ (build 17763) up to pre-22H2 Windows 11 — no DWM
    /// system backdrop yet, but a real blur is still available via the
    /// older, undocumented `SetWindowCompositionAttribute` API's
    /// `ACCENT_ENABLE_ACRYLICBLURBEHIND` state — a genuine Acrylic-style
    /// blur-behind, not a plain translucency.
    Acrylic,
    /// Older, or the build number couldn't be read — no window translucency
    /// is available; the window must stay fully opaque.
    None,
}

/// Detects [`WindowsBackdropSupport`] via `RtlGetVersion` (`GetVersionEx`
/// lies about the OS version to any process without an application
/// manifest declaring Windows 10/11 support — this is the same real-version
/// check gpui's own Windows backend uses internally before deciding whether
/// `DWMWA_SYSTEMBACKDROP_TYPE` is safe to call, in
/// `gpui_windows::window::dwm_set_window_composition_attribute`). Reading
/// gpui's source directly (not guessed) is also where the build-number
/// thresholds above came from: `22621` and `17763` are exactly what
/// `dwm_set_window_composition_attribute` / `set_window_composition_attribute`
/// themselves gate on.
#[cfg(target_os = "windows")]
pub fn windows_backdrop_support() -> WindowsBackdropSupport {
    use windows::Wdk::System::SystemServices::RtlGetVersion;

    // SAFETY: `version` is zero-initialized and `RtlGetVersion` only ever
    // writes into it — the same call gpui's own Windows backend makes
    // (`gpui_windows::window::dwm_set_window_composition_attribute`).
    let mut version = unsafe { std::mem::zeroed() };
    let status = unsafe { RtlGetVersion(&mut version) };
    if !status.is_ok() {
        return WindowsBackdropSupport::None;
    }
    let build: u32 = version.dwBuildNumber;
    if build >= 22621 {
        WindowsBackdropSupport::Mica
    } else if build >= 17763 {
        WindowsBackdropSupport::Acrylic
    } else {
        WindowsBackdropSupport::None
    }
}

#[cfg(not(target_os = "windows"))]
pub fn windows_backdrop_support() -> WindowsBackdropSupport {
    WindowsBackdropSupport::None
}
