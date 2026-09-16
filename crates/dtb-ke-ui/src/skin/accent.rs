//! The live macOS system accent colour (`NSColor.controlAccentColor`), used
//! by [`crate::theme`] when `Settings.use_system_accent_color` is on.
//!
//! Every public fn here is `#[cfg(target_os = "macos")]` with a
//! `#[cfg(not(target_os = "macos"))]` fallback, so callers never need
//! `#[cfg]` — the same convention as [`super::window::windows_backdrop_support`].

use gpui::{App, Hsla};

use crate::theme::Appearance;

/// Read the live system accent colour, resolved under `appearance` (AppKit's
/// dynamic system colours can legitimately differ between light/dark — this
/// app's [`crate::theme::ThemeMode`] can force one appearance independent of
/// the OS's own, so the read has to name which one explicitly rather than
/// relying on whatever the OS currently is). `None` off macOS, with no main
/// thread, or if AppKit resolution fails for any reason — callers fall back
/// to the dtb.toml accent.
#[cfg(target_os = "macos")]
pub fn system_accent(appearance: Appearance) -> Option<Hsla> {
    macos::system_accent(appearance)
}

#[cfg(not(target_os = "macos"))]
pub fn system_accent(_appearance: Appearance) -> Option<Hsla> {
    None
}

/// Start watching `NSSystemColorsDidChangeNotification` and re-apply the
/// theme (via [`crate::theme::Theme::reload`]) whenever the user changes the
/// OS accent colour in System Settings — so the app re-tints live, with no
/// restart. No-op off macOS. Call once at startup, unconditionally; the
/// observer lives for the process (never removed), the same lifetime as the
/// crash handler.
#[cfg(target_os = "macos")]
pub fn watch_system_accent(cx: &mut App) {
    macos::watch_system_accent(cx);
}

#[cfg(not(target_os = "macos"))]
pub fn watch_system_accent(_cx: &mut App) {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::{Cell, RefCell};
    use std::ptr::NonNull;

    use block2::{RcBlock, StackBlock};
    use futures::StreamExt;
    use gpui::{App, Hsla, Rgba};
    use objc2::MainThreadMarker;
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceNameAqua, NSAppearanceNameDarkAqua, NSColor, NSColorSpace,
        NSSystemColorsDidChangeNotification,
    };
    use objc2_foundation::{NSNotification, NSNotificationCenter};

    use crate::theme::{Appearance, Theme};

    thread_local! {
        /// Keeps the notification observer alive for the process; never torn
        /// down (the same "installed once, lives forever" lifetime as the
        /// crash handler).
        static OBSERVER: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> =
            const { RefCell::new(None) };
    }

    pub fn system_accent(appearance: Appearance) -> Option<Hsla> {
        let _mtm = MainThreadMarker::new()?;

        let name = match appearance {
            Appearance::Light => unsafe { NSAppearanceNameAqua },
            Appearance::Dark => unsafe { NSAppearanceNameDarkAqua },
        };
        let ns_appearance = NSAppearance::appearanceNamed(name)?;

        // `controlAccentColor` is a dynamic, catalog-space colour — asking it
        // for RGBA components directly throws. Resolve it to concrete sRGB
        // components while `ns_appearance` is the current drawing appearance
        // (so light/dark variants of the dynamic colour resolve correctly),
        // via the non-escaping block API.
        let result: Cell<Option<Hsla>> = Cell::new(None);
        let block = StackBlock::new(|| {
            let accent = NSColor::controlAccentColor();
            let Some(srgb) = accent.colorUsingColorSpace(&NSColorSpace::sRGBColorSpace()) else {
                return;
            };
            let (mut r, mut g, mut b, mut a) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            // SAFETY: four valid, non-null out-pointers to local `f64`s.
            unsafe { srgb.getRed_green_blue_alpha(&mut r, &mut g, &mut b, &mut a) };
            result.set(Some(Hsla::from(Rgba {
                r: r as f32,
                g: g as f32,
                b: b as f32,
                a: a as f32,
            })));
        });
        ns_appearance.performAsCurrentDrawingAppearance(&block);
        result.into_inner()
    }

    pub fn watch_system_accent(cx: &mut App) {
        let Some(_mtm) = MainThreadMarker::new() else {
            return;
        };

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
            let _ = tx.unbounded_send(());
        });
        // SAFETY: `defaultCenter` + registering a block observer for a
        // well-known system notification name; `block` outlives the
        // registration (the returned token is kept in `OBSERVER` for the
        // process's lifetime, and we never remove the observer).
        let token = unsafe {
            let center = NSNotificationCenter::defaultCenter();
            center.addObserverForName_object_queue_usingBlock(
                Some(NSSystemColorsDidChangeNotification),
                None,
                None,
                &block,
            )
        };
        OBSERVER.with(|cell| *cell.borrow_mut() = Some(token));

        cx.spawn(async move |cx| {
            while rx.next().await.is_some() {
                cx.update(Theme::reload);
            }
        })
        .detach();
    }
}
