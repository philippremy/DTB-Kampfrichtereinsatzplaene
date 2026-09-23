//! The live macOS app icon. Tahoe+'s Liquid Glass icons render differently
//! depending on System Settings → Appearance → Icon & Widget style
//! (Light/Dark/Clear/Tinted, plus a tint-intensity slider) — this fetches
//! the *current* rendition and watches for the user changing it live, so
//! [`crate::about`] never needs its own embedded copy of the icon on macOS
//! (Windows/Linux still embed one via `include_bytes!` — there is no
//! equivalent native fetch there).
//!
//! Every public fn here is `#[cfg(target_os = "macos")]` with a no-op
//! fallback for every other OS, the same convention as [`super::accent`].
//!
//! Findings this module is built on (empirically verified against a real
//! Tahoe+ build of this app — see the "About window icon" work item):
//! - `NSImage(named: NSImage.applicationIconName)` never returns `nil`, even
//!   for a bare, non-bundled debug binary with no `Info.plist` at all — it
//!   falls back to a generic system icon (confirmed: a plain folder glyph).
//!   Once properly bundled (`CFBundleIconName` pointing at the Icon
//!   Composer-derived `Assets.car` — see `dtb-ke-bundle`'s `icon.rs`), it
//!   resolves the real icon and *is* live appearance-reactive.
//! - `NSApplication.applicationIconImage`, by contrast, is a **fixed
//!   snapshot** — it does not update when the user changes the icon
//!   appearance preference. Do not use it for this purpose.
//! - `-[NSImage CGImageForProposedRect:context:hints:]`, rasterised straight
//!   to a BGRA8 buffer through a `CGContext` (bypassing `NSBitmapImageRep`'s
//!   PNG/TIFF encoders entirely — no `image`-crate format decoder needs to
//!   be linked in for this path), is *also* live appearance-reactive —
//!   empirically confirmed identically to the plain `NSImage` fetch above.
//!   `CGBitmapContextCreate` only accepts **premultiplied** alpha formats
//!   (`.premultipliedFirst | .byteOrder32Little`, giving BGRA8 in memory —
//!   exactly gpui's `RenderImage` layout, see `preview.rs`'s own BGRA
//!   pipeline), so the result is unpremultiplied by hand afterward: gpui's
//!   `polychrome_sprites` pipeline (the one that draws `RenderImage`
//!   content) blends with straight, not premultiplied, alpha.
//! - There is a private (no public header, but a real exported linker
//!   symbol in `AppKit.framework`) notification,
//!   `NSWorkspaceIconAppearanceConfigurationDidChangeNotification`, posted
//!   on `NSWorkspace.shared.notificationCenter`. It only fires for a
//!   process running a real `NSApplication` event loop — a bare
//!   command-line tool pumping just a `RunLoop` never receives it (gpui's
//!   own app always runs a real `NSApplication`, so this is a non-issue
//!   here).

use gpui_kit::App;

/// The current app icon, rasterised to straight-alpha BGRA8 at a fixed
/// `SIZE × SIZE` — `(width, height, bgra_bytes)`. Always `Some` in practice
/// — see the module doc's first finding. `None` only if this somehow isn't
/// called on the main thread, or AppKit's documented fallback contract is
/// broken.
#[cfg(target_os = "macos")]
pub fn current_icon_bgra() -> Option<(u32, u32, Vec<u8>)> {
    macos::current_icon_bgra()
}
#[cfg(not(target_os = "macos"))]
pub fn current_icon_bgra() -> Option<(u32, u32, Vec<u8>)> {
    None
}

/// Start watching macOS 26+'s live icon-appearance preference and call
/// [`crate::about::refresh_icon`] (on the main thread) whenever the user
/// changes it, so the About window's icon updates with no restart. No-op
/// off macOS. Call once at startup, unconditionally; the observer lives for
/// the process (never removed) — the same lifetime as
/// `skin::accent::watch_system_accent` and the crash handler.
#[cfg(target_os = "macos")]
pub fn watch(cx: &mut App) {
    macos::watch(cx);
}
#[cfg(not(target_os = "macos"))]
pub fn watch(_cx: &mut App) {}

#[cfg(target_os = "macos")]
mod macos {
    use std::cell::RefCell;
    use std::ptr::NonNull;

    use block2::RcBlock;
    use futures::StreamExt;
    use gpui_kit::App;
    use objc2::{AnyThread, MainThreadMarker};
    use objc2::rc::Retained;
    use objc2::runtime::{NSObjectProtocol, ProtocolObject};
    use objc2_app_kit::{NSBundleImageExtension, NSImage, NSImageNameApplicationIcon, NSWorkspace};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_core_graphics::{
        CGBitmapContextCreate, CGColorSpace, CGContext, CGImageAlphaInfo, CGImageByteOrderInfo,
    };
    use objc2_foundation::{NSBundle, NSNotification, NSString, ns_string};

    use crate::about;

    thread_local! {
        /// Keeps the notification observer alive for the process; never torn
        /// down — the same "installed once, lives forever" lifetime as
        /// `skin::accent`'s own observer.
        static OBSERVER: RefCell<Option<Retained<ProtocolObject<dyn NSObjectProtocol>>>> =
            const { RefCell::new(None) };
    }

    /// See the module doc's third finding for why this has no typed
    /// binding: it's real (an exported `AppKit.framework` symbol), just
    /// undocumented.
    const ICON_APPEARANCE_DID_CHANGE: &str =
        "NSWorkspaceIconAppearanceConfigurationDidChangeNotification";

    /// Raster target — @2x of the 128pt size the About window draws the
    /// icon at, since gpui scales the source pixels to fit the layout box
    /// (no separate "retina" asset variant needed).
    const SIZE: u32 = 256;

    pub fn current_icon_bgra() -> Option<(u32, u32, Vec<u8>)> {
        // We switch on whether the application is bundled
        let ns_image = match NSBundle::mainBundle().bundleIdentifier() {
            Some(_) => {
                // SAFETY: `NSImageNameApplicationIcon` is a real, always-initialised
                // `AppKit.framework` constant string.
                let name = unsafe { NSImageNameApplicationIcon };
                NSImage::imageNamed(name)?
            },
            None => {
                // We get the icon from the private IconFoundation framework
                let bundle = NSBundle::initWithPath(NSBundle::alloc(), ns_string!("/System/Library/PrivateFrameworks/IconFoundation.framework"))?;
                bundle.imageForResource(ns_string!("com.apple.application-bundle"))?
            }
        };
        let mut rect = CGRect::new(CGPoint::ZERO, CGSize::new(SIZE as f64, SIZE as f64));
        // SAFETY: `&mut rect` is a valid, live pointer for the call's
        // duration; `context`/`hints` are both `None`, a documented-valid
        // input (AppKit picks a default graphics context / no hints).
        let cg_image = unsafe { ns_image.CGImageForProposedRect_context_hints(&mut rect, None, None) }?;

        let mut buf = vec![0u8; (SIZE * SIZE * 4) as usize];
        let color_space = CGColorSpace::new_device_rgb()?;
        // `CGBitmapContextCreate` only accepts a handful of *premultiplied*
        // (or alpha-less) formats — see the module doc. Memory byte order
        // for `.premultipliedFirst | .order32Little` is B,G,R,A.
        let bitmap_info = CGImageAlphaInfo::PremultipliedFirst.0 | CGImageByteOrderInfo::Order32Little.0;
        // SAFETY: `buf` is a real, correctly-sized (`SIZE*SIZE*4` bytes)
        // buffer that outlives `context` (dropped right after, below);
        // `CGBitmapContextCreate` (unlike `…CreateWithData`) never takes
        // ownership of a caller-supplied buffer, so Rust keeps owning it.
        let context = unsafe {
            CGBitmapContextCreate(
                buf.as_mut_ptr().cast(),
                SIZE as usize,
                SIZE as usize,
                8,
                (SIZE * 4) as usize,
                Some(&color_space),
                bitmap_info,
            )
        }?;
        let draw_rect = CGRect::new(CGPoint::ZERO, CGSize::new(SIZE as f64, SIZE as f64));
        CGContext::draw_image(Some(&context), draw_rect, Some(&cg_image));
        drop(context);

        unpremultiply_bgra(&mut buf);
        Some((SIZE, SIZE, buf))
    }

    /// `CGBitmapContextCreate` only draws into premultiplied buffers (see
    /// `current_icon_bgra`), but gpui's `polychrome_sprites` pipeline (the
    /// one `RenderImage` content draws through) blends straight alpha —
    /// premultiplied input would double-darken every anti-aliased/
    /// semi-transparent edge. `buf` is BGRA8; only the per-channel math
    /// matters here, not which byte is which color.
    fn unpremultiply_bgra(buf: &mut [u8]) {
        for px in buf.as_chunks_mut::<4>().0 {
            let a = px[3];
            if a == 0 {
                px[0] = 0;
                px[1] = 0;
                px[2] = 0;
            } else if a != 255 {
                for c in &mut px[..3] {
                    *c = ((*c as u32 * 255) / a as u32).min(255) as u8;
                }
            }
        }
    }

    pub fn watch(cx: &mut App) {
        let Some(_mtm) = MainThreadMarker::new() else {
            return;
        };

        let (tx, mut rx) = futures::channel::mpsc::unbounded::<()>();
        let block = RcBlock::new(move |_note: NonNull<NSNotification>| {
            let _ = tx.unbounded_send(());
        });
        let name = NSString::from_str(ICON_APPEARANCE_DID_CHANGE);
        // SAFETY: registering a block observer for a real (if undocumented)
        // notification name on `NSWorkspace`'s own center; `block` outlives
        // the registration (the returned token is kept in `OBSERVER` for the
        // process's lifetime, and we never remove the observer) — the same
        // pattern as `skin::accent::watch_system_accent`.
        let token = unsafe {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            center.addObserverForName_object_queue_usingBlock(Some(&name), None, None, &block)
        };
        OBSERVER.with(|cell| *cell.borrow_mut() = Some(token));

        cx.spawn(async move |cx| {
            while rx.next().await.is_some() {
                cx.update(about::refresh_icon);
            }
        })
        .detach();
    }
}
