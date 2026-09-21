//! The native window backdrop (macOS).
//!
//! gpui's own `WindowBackgroundAppearance::Blurred` stacks an
//! `NSVisualEffectView` (material `Selection`) under its view and strips the
//! material's private tint layers so only the blur remains. macOS 27 dropped
//! the blur layer (`CABackdropLayer`) from `Selection`, so that path paints
//! nothing there (zed-industries/zed#64284, closed not-planned). Instead of
//! patching gpui, the window is requested `Transparent` and we place our own
//! native views directly beneath gpui's view — the same slot gpui used —
//! left as AppKit builds them, so they follow Reduce Transparency, the
//! system's Clear/Tinted glass setting and the window's active state.
//!
//! Two tiers, picked once per window at [`install`]:
//! * **Vibrancy** (macOS before 26): one window-filling `NSVisualEffectView`
//!   (`Sidebar` material). Chrome that should show it just paints no fill.
//! * **Glass** (macOS 26+): an `NSGlassEffectContainerView` holding one
//!   `NSGlassEffectView` per [`GlassRegion`] that views declare through
//!   [`super::glass`]; nothing is drawn outside those regions.
//!
//! The native views' appearance is pinned to the app's [`ThemeMode`] so a
//! forced dark palette never sits on a light material; `System` leaves it
//! unpinned and it follows the OS. Only the main window has a backdrop. Off
//! macOS every fn is a no-op.

use gpui::Window;

use crate::theme::ThemeMode;

use super::glass::Regions;

/// Whether this platform can host a native backdrop at all.
pub fn available() -> bool {
    cfg!(target_os = "macos")
}

/// Whether the installed backdrop is the glass tier — i.e. whether glass
/// regions should be recorded and applied at all.
pub fn glass_active() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::glass_active()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
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

/// Makes the glass views match `regions` (the tier-`Glass` backdrop only).
pub(super) fn apply_regions(regions: &Regions) {
    #[cfg(target_os = "macos")]
    mac::apply_regions(regions);
    #[cfg(not(target_os = "macos"))]
    let _ = regions;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use gpui::Window;
    use log::{debug, warn};
    use objc2::rc::Retained;
    use objc2::runtime::{AnyClass, AnyObject};
    use objc2::{MainThreadMarker, MainThreadOnly, define_class, msg_send};
    use objc2_app_kit::{
        NSAppearance, NSAppearanceCustomization, NSAppearanceNameAqua, NSAppearanceNameDarkAqua,
        NSAutoresizingMaskOptions, NSBox, NSBoxType, NSColor, NSGlassEffectView,
        NSGlassEffectViewStyle, NSTitlePosition, NSView, NSVisualEffectBlendingMode,
        NSVisualEffectMaterial, NSVisualEffectState, NSVisualEffectView, NSWindowOrderingMode,
    };
    use objc2_foundation::{NSPoint, NSRect, NSSize};
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Regions;
    use crate::skin::glass::{GlassRegion, GlassStyle};
    use crate::theme::ThemeMode;

    define_class!(
        /// A plain `NSView` with a top-left origin. The glass holder uses it so
        /// regions stay anchored to the top while AppKit resizes the window —
        /// gpui only re-lays-out a frame later, and a bottom-left origin made
        /// full-height regions drift for that frame.
        // SAFETY: `NSView` has no subclassing requirements, and `FlippedView`
        // adds no ivars and doesn't implement `Drop`.
        #[unsafe(super(NSView))]
        #[thread_kind = MainThreadOnly]
        #[name = "DTBKeFlippedView"]
        struct FlippedView;

        impl FlippedView {
            #[unsafe(method(isFlipped))]
            fn is_flipped(&self) -> bool {
                true
            }
        }
    );

    impl FlippedView {
        fn new(mtm: MainThreadMarker, frame: NSRect) -> Retained<Self> {
            let this = mtm.alloc::<Self>().set_ivars(());
            unsafe { msg_send![super(this), initWithFrame: frame] }
        }
    }

    /// One region's native views, kept alive while the region is absent
    /// (hidden) so bringing it back doesn't replay the glass's own
    /// materialise animation.
    struct GlassEntry {
        backing: Retained<NSBox>,
        glass: Retained<NSGlassEffectView>,
        last: GlassRegion,
    }

    /// The glass tier's native views.
    struct GlassHost {
        /// `NSGlassEffectContainerView` — merges nearby glass and shares one
        /// render pass. Sits in the window's content view under gpui's view.
        container: Retained<NSView>,
        /// The container's `contentView`; the glass views are its subviews.
        holder: Retained<NSView>,
        views: HashMap<&'static str, GlassEntry>,
    }

    enum Kind {
        Vibrancy(Retained<NSVisualEffectView>),
        Glass(GlassHost),
    }

    struct State {
        /// gpui's parent view (the window's content view).
        content: Retained<NSView>,
        kind: Kind,
    }

    thread_local! {
        /// The main window's backdrop, kept so a theme-mode change can re-pin
        /// it and the glass regions can be applied without going through a
        /// (possibly re-entrant) window update.
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    pub fn glass_active() -> bool {
        STATE.with(|s| matches!(s.borrow().as_ref(), Some(State { kind: Kind::Glass(_), .. })))
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
        let Some(gpui_view) = (unsafe { Retained::retain(handle.ns_view.as_ptr().cast::<NSView>()) })
        else {
            return;
        };
        // gpui's view is a subview of the window's plain content view; the
        // backdrop goes into the same parent, beneath it.
        let Some(content) = (unsafe { gpui_view.superview() }) else {
            warn!("native backdrop: gpui view has no superview");
            return;
        };

        let kind = match glass_host(mtm, &content) {
            Some(host) => {
                debug!("native backdrop installed (glass tier)");
                Kind::Glass(host)
            }
            None => {
                let view = NSVisualEffectView::initWithFrame(mtm.alloc(), content.bounds());
                view.setMaterial(NSVisualEffectMaterial::Sidebar);
                view.setBlendingMode(NSVisualEffectBlendingMode::BehindWindow);
                view.setState(NSVisualEffectState::FollowsWindowActiveState);
                view.setAutoresizingMask(fill_mask());
                content.addSubview_positioned_relativeTo(&view, NSWindowOrderingMode::Below, None);
                debug!("native backdrop installed (vibrancy tier, sidebar material)");
                Kind::Vibrancy(view)
            }
        };
        let state = State { content, kind };
        pin_appearance(&state, mode);
        STATE.with(|s| *s.borrow_mut() = Some(state));
    }

    pub fn remove() {
        STATE.with(|s| {
            if let Some(state) = s.borrow_mut().take() {
                match &state.kind {
                    Kind::Vibrancy(view) => view.removeFromSuperview(),
                    Kind::Glass(host) => host.container.removeFromSuperview(),
                }
                debug!("native backdrop removed");
            }
        });
    }

    pub fn set_mode(mode: ThemeMode) {
        STATE.with(|s| {
            if let Some(state) = s.borrow().as_ref() {
                pin_appearance(state, mode);
            }
        });
    }

    pub fn apply_regions(regions: &Regions) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let Some(State {
                content,
                kind: Kind::Glass(host),
            }) = s.as_mut()
            else {
                return;
            };
            let size = content.bounds().size;

            // No implicit Core Animation motion on any of this.
            begin_no_actions();
            for (id, entry) in host.views.iter() {
                if !regions.contains_key(id) {
                    entry.backing.setHidden(true);
                    entry.glass.setHidden(true);
                }
            }
            for (id, region) in regions {
                if !host.views.contains_key(id) {
                    let frame = ns_frame(region);
                    let backing = NSBox::initWithFrame(mtm.alloc(), frame);
                    backing.setBoxType(NSBoxType::Custom);
                    backing.setTitlePosition(NSTitlePosition::NoTitle);
                    backing.setBorderWidth(0.0);
                    backing.setCornerRadius(0.0);
                    let glass = NSGlassEffectView::initWithFrame(mtm.alloc(), frame);
                    host.holder.addSubview(&backing);
                    host.holder.addSubview(&glass);
                    host.views.insert(
                        id,
                        GlassEntry {
                            backing,
                            glass,
                            // Forces the first full apply below.
                            last: GlassRegion {
                                bounds: gpui::Bounds::default(),
                                ..*region
                            },
                        },
                    );
                    // A brand-new pair must be applied even if `region` equals
                    // the placeholder.
                    if let Some(e) = host.views.get(id) {
                        e.backing.setHidden(true);
                    }
                }
                let entry = host.views.get_mut(id).expect("just inserted");
                let unchanged = entry.last == *region && !entry.glass.isHidden();
                if unchanged {
                    continue;
                }
                let frame = ns_frame(region);
                let mask = edge_mask(region, size);
                for view in [&*entry.backing as &NSView, &*entry.glass as &NSView] {
                    if view.frame() != frame {
                        view.setFrame(frame);
                    }
                    if view.autoresizingMask() != mask {
                        view.setAutoresizingMask(mask);
                    }
                    view.setHidden(false);
                }
                entry.backing.setHidden(region.backing.a <= 0.0);
                entry.backing.setFillColor(&ns_color(region.backing));

                entry.glass.setStyle(match region.style {
                    GlassStyle::Regular => NSGlassEffectViewStyle::Regular,
                    GlassStyle::Clear => NSGlassEffectViewStyle::Clear,
                });
                entry
                    .glass
                    .setCornerRadius(f64::from(f32::from(region.corner_radius)));
                entry.glass.setTintColor(
                    (region.tint.a > 0.0)
                        .then(|| ns_color(region.tint))
                        .as_deref(),
                );
                entry.last = *region;
            }
            end_no_actions();
        });
    }

    /// gpui's top-left-origin window space; the holder is flipped, so it maps
    /// straight across.
    fn ns_frame(region: &GlassRegion) -> NSRect {
        let b = region.bounds;
        let f = |p: gpui::Pixels| f64::from(f32::from(p));
        NSRect::new(
            NSPoint::new(f(b.origin.x), f(b.origin.y)),
            NSSize::new(f(b.size.width), f(b.size.height)),
        )
    }

    /// A region that spans the window's full width and/or height keeps doing
    /// so while AppKit resizes the window, until gpui's next frame re-applies it.
    fn edge_mask(region: &GlassRegion, window: NSSize) -> NSAutoresizingMaskOptions {
        let b = region.bounds;
        let f = |p: gpui::Pixels| f64::from(f32::from(p));
        let mut mask = NSAutoresizingMaskOptions::empty();
        if f(b.origin.x) <= 0.5 && f(b.origin.x + b.size.width) >= window.width - 0.5 {
            mask |= NSAutoresizingMaskOptions::ViewWidthSizable;
        }
        if f(b.origin.y) <= 0.5 && f(b.origin.y + b.size.height) >= window.height - 0.5 {
            mask |= NSAutoresizingMaskOptions::ViewHeightSizable;
        }
        mask
    }

    fn ns_color(c: gpui::Hsla) -> Retained<NSColor> {
        let rgba = gpui::Rgba::from(c);
        NSColor::colorWithSRGBRed_green_blue_alpha(
            f64::from(rgba.r),
            f64::from(rgba.g),
            f64::from(rgba.b),
            f64::from(rgba.a),
        )
    }

    fn begin_no_actions() {
        if let Some(class) = AnyClass::get(c"CATransaction") {
            // SAFETY: `+begin` / `+setDisableActions:` are CATransaction's
            // documented class methods; balanced by `end_no_actions`.
            unsafe {
                let _: () = msg_send![class, begin];
                let _: () = msg_send![class, setDisableActions: true];
            }
        }
    }

    fn end_no_actions() {
        if let Some(class) = AnyClass::get(c"CATransaction") {
            // SAFETY: pairs with `begin_no_actions`.
            unsafe {
                let _: () = msg_send![class, commit];
            }
        }
    }

    fn fill_mask() -> NSAutoresizingMaskOptions {
        NSAutoresizingMaskOptions::ViewWidthSizable | NSAutoresizingMaskOptions::ViewHeightSizable
    }

    /// Builds the glass container (macOS 26+; `None` when the class is
    /// missing). `NSGlassEffectContainerView` has no typed binding in
    /// objc2-app-kit yet, so it goes through the runtime.
    fn glass_host(mtm: MainThreadMarker, content: &NSView) -> Option<GlassHost> {
        let class = AnyClass::get(c"NSGlassEffectContainerView")?;
        AnyClass::get(c"NSGlassEffectView")?;
        let frame = content.bounds();
        // SAFETY: `NSGlassEffectContainerView` is an `NSView` subclass;
        // `initWithFrame:` / `setContentView:` / `setSpacing:` are its
        // documented API, with the argument types used here.
        let container: Retained<NSView> = unsafe {
            let alloc: *mut AnyObject = msg_send![class, alloc];
            let raw: *mut AnyObject = msg_send![alloc, initWithFrame: frame];
            Retained::from_raw(raw.cast::<NSView>())?
        };
        let holder = FlippedView::new(mtm, frame).into_super();
        holder.setAutoresizingMask(fill_mask());
        unsafe {
            let _: () = msg_send![&*container, setContentView: &*holder];
            // Glass regions merge when closer than this (points); the sidebar
            // is alone, so 0 keeps unrelated surfaces from fusing.
            let _: () = msg_send![&*container, setSpacing: 0.0f64];
        }
        container.setAutoresizingMask(fill_mask());
        content.addSubview_positioned_relativeTo(&container, NSWindowOrderingMode::Below, None);
        Some(GlassHost {
            container,
            holder,
            views: HashMap::new(),
        })
    }

    fn pin_appearance(state: &State, mode: ThemeMode) {
        let appearance = match mode {
            ThemeMode::System => None,
            ThemeMode::Light => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameAqua }),
            ThemeMode::Dark => NSAppearance::appearanceNamed(unsafe { NSAppearanceNameDarkAqua }),
        };
        match &state.kind {
            Kind::Vibrancy(view) => view.setAppearance(appearance.as_deref()),
            Kind::Glass(host) => host.container.setAppearance(appearance.as_deref()),
        }
    }
}
