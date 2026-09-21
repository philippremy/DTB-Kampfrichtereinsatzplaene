//! The native window backdrop (macOS).
//!
//! gpui's own `WindowBackgroundAppearance::Blurred` stacks an
//! `NSVisualEffectView` (material `Selection`) under its view and strips the
//! material's private tint layers so only the blur remains. macOS 27 dropped
//! the blur layer (`CABackdropLayer`) from `Selection`, so that path paints
//! nothing there (zed-industries/zed#64284, closed not-planned). Instead of
//! patching gpui, the window is requested `Transparent` and we place our own,
//! **unstripped** `NSVisualEffectView` (`Sidebar` material, behind-window
//! blending) directly beneath gpui's view — the same slot gpui used. Left as
//! AppKit builds it, the material follows the system's Reduce Transparency
//! setting, the macOS 27 transparency slider and the window's active state.
//!
//! The view's appearance is pinned to the app's [`ThemeMode`] so a forced
//! dark palette never sits on a light material; `System` leaves it unpinned
//! and it follows the OS.
//!
//! Only the main window has one. Off macOS every fn is a no-op.

use gpui::Window;

use crate::theme::ThemeMode;

/// Whether this platform can host the native backdrop at all.
pub fn available() -> bool {
    cfg!(target_os = "macos")
}

/// Installs the backdrop under `window`'s gpui view (idempotent — replaces an
/// earlier one) and pins it to `mode`.
pub fn install(window: &Window, mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    mac::install(window, mode);
    #[cfg(not(target_os = "macos"))]
    let _ = (window, mode);
}

/// Removes the backdrop, if any.
pub fn remove() {
    #[cfg(target_os = "macos")]
    mac::remove();
}

/// Re-pins the installed backdrop's appearance after a theme-mode change.
pub fn set_mode(mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    mac::set_mode(mode);
    #[cfg(not(target_os = "macos"))]
    let _ = mode;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;

    use gpui::Window;
    use log::{debug, warn};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSAutoresizingMaskOptions, NSView, NSVisualEffectBlendingMode, NSVisualEffectMaterial,
        NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use crate::theme::ThemeMode;

    thread_local! {
        /// The main window's backdrop, kept so a theme-mode change can re-pin
        /// it without going through a (possibly re-entrant) window update.
        static VIEW: RefCell<Option<Retained<NSVisualEffectView>>> = const { RefCell::new(None) };
    }

    pub fn install(window: &Window, mode: ThemeMode) {
        let Some(mtm) = MainThreadMarker::new() else {
            warn!("native backdrop: not on the main thread");
            return;
        };
        remove();
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            warn!("native backdrop: window has no raw handle");
            return;
        };
        let RawWindowHandle::AppKit(handle) = handle.as_raw() else {
            warn!("native backdrop: unexpected raw window handle kind");
            return;
        };
        // SAFETY: gpui documents the AppKit handle as a pointer to its own
        // (live) `NSView`; we're on the main thread and only retain it.
        let Some(gpui_view) =
            (unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) })
        else {
            return;
        };
        // gpui's view is a subview of the window's plain content view; the
        // backdrop goes into the same parent, beneath it.
        let Some(content) = (unsafe { gpui_view.superview() }) else {
            warn!("native backdrop: gpui view has no superview");
            return;
        };

        let view = NSVisualEffectView::initWithFrame(mtm.alloc(), content.bounds());
        view.setMaterial(NSVisualEffectMaterial::Sidebar);
        view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
        view.setState(NSVisualEffectState::FollowsWindowActiveState);
        view.setAutoresizingMask(
            NSAutoresizingMaskOptions::ViewWidthSizable
                | NSAutoresizingMaskOptions::ViewHeightSizable,
        );
        pin_appearance(&view, mode);
        content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
        debug!("native backdrop installed (sidebar material)");
        VIEW.with(|v| *v.borrow_mut() = Some(view));
    }

    pub fn remove() {
        VIEW.with(|v| {
            if let Some(view) = v.borrow_mut().take() {
                view.removeFromSuperview();
                debug!("native backdrop removed");
            }
        });
    }

    pub fn set_mode(mode: ThemeMode) {
        VIEW.with(|v| {
            if let Some(view) = v.borrow().as_ref() {
                pin_appearance(view, mode);
            }
        });
    }

    fn pin_appearance(view: &NSVisualEffectView, mode: ThemeMode) {
        let appearance = match mode {
            ThemeMode::System => None,
            ThemeMode::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
            ThemeMode::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
        };
        view.setAppearance(appearance.as_deref());
    }
}
