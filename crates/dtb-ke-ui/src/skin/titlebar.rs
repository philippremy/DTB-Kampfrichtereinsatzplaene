//! Title-bar geometry that depends on the OS window frame.

use gpui::{Pixels, Window, px};

/// Left inset the title-bar content needs so it clears the OS window controls.
///
/// macOS draws the traffic lights *inside* our transparent title bar, so the
/// content must start to their right. Windows and Linux keep their own title
/// bar above ours (until Phases 7/8), so only a small gutter is needed.
// `window` is only read on the macOS branch below (the traffic-lights inset);
// on Windows/Linux it's unused, which only ever warns on a build that
// actually compiles that branch out — hence this only ever showed up on the
// Linux runner, never locally on macOS.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn content_leading_inset(window: &mut Window) -> Pixels {
    #[cfg(target_os = "macos")]
    {
        if window.is_fullscreen() {
            px(12.)
        } else {
            px(82.)
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        px(12.)
    }
}

/// Whether our title bar must drive window dragging + the min/max/close
/// controls itself — true only under client-side decorations (Linux). macOS
/// drags via its transparent OS title bar; Windows keeps the OS frame.
pub fn owns_window_chrome(window: &Window) -> bool {
    crate::skin::decorations::is_client(window)
}
