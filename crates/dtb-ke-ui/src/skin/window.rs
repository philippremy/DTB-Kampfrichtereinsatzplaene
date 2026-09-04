//! Window construction, derived from the active skin.

use gpui::{App, Bounds, Size, TitlebarOptions, WindowBounds, WindowOptions, point, px};

use crate::material;
use crate::skin::decorations;
use crate::theme::{ActiveTheme, WindowDecorations};

/// Options for the app's main window.
///
/// The window background follows the active skin's material (`Blurred` on macOS,
/// `MicaBackdrop` on Windows, opaque on Linux); the Linux skin also requests
/// client-side decorations so the app draws its own frame (see
/// [`crate::skin::decorations`]).
pub fn main_window_options(cx: &mut App) -> WindowOptions {
    let theme = cx.theme();
    let window_background = material::window_background(theme);
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
