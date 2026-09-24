//! The native window backdrop (macOS, and iPadOS's UIKit glass — see the `ios` module).
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

use gpui_kit::Window;

use crate::theme::ThemeMode;

use super::glass::Regions;

/// Whether this platform can host a native backdrop at all.
pub fn available() -> bool {
    #[cfg(target_os = "ios")]
    {
        ios::available()
    }
    #[cfg(not(target_os = "ios"))]
    {
        cfg!(target_os = "macos")
    }
}

/// A blurred material was requested but this platform has no way to provide it (iPadOS before
/// glass) — the caller should fall back to opaque surfaces rather than gpui's own blur.
pub fn blur_unsupported() -> bool {
    cfg!(target_os = "ios") && !available()
}

/// Whether the installed backdrop is the glass tier — i.e. whether glass
/// regions should be recorded and applied at all.
pub fn glass_active() -> bool {
    #[cfg(target_os = "macos")]
    {
        mac::glass_active()
    }
    #[cfg(target_os = "ios")]
    {
        ios::glass_active()
    }
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    {
        false
    }
}

/// Installs the backdrop under `window`'s gpui view (idempotent — replaces an
/// earlier one) and pins it to `mode`.
pub fn install(window: &Window, mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    mac::install(window, mode);
    #[cfg(target_os = "ios")]
    ios::install(window, mode);
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = (window, mode);
}

/// Removes the backdrop, if any.
pub fn remove() {
    #[cfg(target_os = "macos")]
    mac::remove();
    #[cfg(target_os = "ios")]
    ios::remove();
}

/// Re-pins the installed backdrop's appearance after a theme-mode change.
pub fn set_mode(mode: ThemeMode) {
    #[cfg(target_os = "macos")]
    mac::set_mode(mode);
    #[cfg(target_os = "ios")]
    ios::set_mode(mode);
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = mode;
}

/// Makes the glass views match `regions` (the tier-`Glass` backdrop only).
pub(super) fn apply_regions(regions: &Regions) {
    #[cfg(target_os = "macos")]
    mac::apply_regions(regions);
    #[cfg(target_os = "ios")]
    ios::apply_regions(regions);
    #[cfg(not(any(target_os = "macos", target_os = "ios")))]
    let _ = regions;
}

#[cfg(target_os = "macos")]
mod mac {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use gpui_kit::{Bounds, Pixels, SharedString, Window};
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
        /// Standalone glass sits in a clipping wrapper, so its drop shadow
        /// can't spill past the region into the transparent gpui area beside
        /// it (the toolbar band) — where nothing would hide it. For a region
        /// with [`GlassRegion::scroll_clip`] set, this wrapper is instead
        /// sized to the scroll pane's own visible bounds and the glass is
        /// positioned inside it relative to that pane — so the wrapper masks
        /// the card the same way gpui's own `overflow_y_scroll` clips its
        /// sibling gpui content.
        clip: Option<Retained<NSView>>,
        last: GlassRegion,
        /// Whether the views are currently shown (vs. hidden for an absent region).
        shown: bool,
    }

    /// The glass tier's native views.
    struct GlassHost {
        /// `NSGlassEffectContainerView` — merges nearby glass and shares one
        /// render pass. Sits in the window's content view under gpui's view.
        container: Retained<NSView>,
        /// The container's `contentView`; the glass views are its subviews.
        holder: Retained<NSView>,
        /// Holds the glass that stands outside the container (the sidebar,
        /// detail-view cards) — sits *below* `container`.
        direct_holder: Retained<NSView>,
        /// Holds standalone glass that must render *above* `container`
        /// instead — currently just a segmented control's [`GlassRole::SelectionThumb`],
        /// which would otherwise blend into an overlapping merged region
        /// instead of reading as its own accent-coloured glass.
        top_holder: Retained<NSView>,
        /// Sits *below* the container: every region's flat backing fill, so a
        /// backing (the toolbar band) can never cover another region's glass.
        backing_holder: Retained<NSView>,
        views: HashMap<SharedString, GlassEntry>,
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
        STATE.with(|s| {
            matches!(
                s.borrow().as_ref(),
                Some(State {
                    kind: Kind::Glass(_),
                    ..
                })
            )
        })
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
                    Kind::Glass(host) => {
                        host.container.removeFromSuperview();
                        host.direct_holder.removeFromSuperview();
                        host.top_holder.removeFromSuperview();
                        host.backing_holder.removeFromSuperview();
                    }
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
            for (id, entry) in host.views.iter_mut() {
                if entry.shown && !regions.contains_key(id) {
                    entry.backing.setHidden(true);
                    entry.glass.setHidden(true);
                    if let Some(clip) = &entry.clip {
                        clip.setHidden(true);
                    }
                    entry.shown = false;
                }
            }
            for (id, region) in regions {
                if !host.views.contains_key(id) {
                    let frame = ns_rect(region.bounds);
                    let backing = NSBox::initWithFrame(mtm.alloc(), frame);
                    backing.setBoxType(NSBoxType::Custom);
                    backing.setTitlePosition(NSTitlePosition::NoTitle);
                    backing.setBorderWidth(0.0);
                    backing.setCornerRadius(0.0);
                    let glass = NSGlassEffectView::initWithFrame(mtm.alloc(), frame);
                    host.backing_holder.addSubview(&backing);
                    let clip = if region.contained {
                        host.holder.addSubview(&glass);
                        None
                    } else {
                        // Top-left origin, like `holder`/`direct_holder`/
                        // `backing_holder` — required so a scroll-clipped
                        // region's glass, positioned at a *non-zero* offset
                        // inside this wrapper (see the `scroll_clip` branch
                        // below), lands using the same coordinate space as
                        // everywhere else instead of AppKit's default
                        // bottom-left one. A plain (unflipped) `NSView` only
                        // ever happened to look right before because the
                        // glass used to fill it exactly (origin `(0, 0)`),
                        // where flipped-vs-not makes no visible difference.
                        let clip = FlippedView::new(mtm, frame).into_super();
                        clip.setClipsToBounds(true);
                        clip.addSubview(&glass);
                        if region.top {
                            host.top_holder.addSubview(&clip);
                        } else {
                            host.direct_holder.addSubview(&clip);
                        }
                        Some(clip)
                    };
                    host.views.insert(
                        id.clone(),
                        GlassEntry {
                            backing,
                            glass,
                            clip,
                            // Forces the first full apply below.
                            last: region.clone(),
                            // Not shown yet, so the first pass below applies it.
                            shown: false,
                        },
                    );
                }
                let entry = host.views.get_mut(id).expect("just inserted");
                if entry.shown && entry.last == *region {
                    continue;
                }
                // The backing always sits at the region's own, unclipped
                // window-relative frame — harmless even for a scroll-clipped
                // card, whose backing is untinted (`a == 0`, see
                // `GlassRole::colors`) and thus hidden below anyway.
                let frame = ns_rect(region.bounds);
                let backing_mask = edge_mask(region.bounds, size);
                if entry.backing.frame() != frame {
                    entry.backing.setFrame(frame);
                }
                if entry.backing.autoresizingMask() != backing_mask {
                    entry.backing.setAutoresizingMask(backing_mask);
                }

                // The view that carries the region's frame: the clip wrapper for
                // standalone glass, else the glass itself.
                if let Some(clip) = &entry.clip {
                    clip.setHidden(false);
                    match &region.scroll_clip {
                        Some((_, viewport)) => {
                            // The wrapper becomes the scroll pane's own
                            // visible rect (shared by every card in the
                            // group, not just this one), and the glass sits
                            // inside it at the card's position *relative to
                            // that pane* — so AppKit's own clipping hides
                            // whatever scrolls past the pane's edges, the
                            // same way gpui's `overflow_y_scroll` clips the
                            // sibling gpui content.
                            let clip_frame = ns_rect(*viewport);
                            if clip.frame() != clip_frame {
                                clip.setFrame(clip_frame);
                            }
                            let empty = NSAutoresizingMaskOptions::empty();
                            if clip.autoresizingMask() != empty {
                                clip.setAutoresizingMask(empty);
                            }
                            let inner = NSRect::new(
                                NSPoint::new(
                                    f64::from(f32::from(
                                        region.bounds.origin.x - viewport.origin.x,
                                    )),
                                    f64::from(f32::from(
                                        region.bounds.origin.y - viewport.origin.y,
                                    )),
                                ),
                                frame.size,
                            );
                            if entry.glass.frame() != inner {
                                entry.glass.setFrame(inner);
                            }
                            entry.glass.setAutoresizingMask(empty);
                        }
                        None => {
                            if clip.frame() != frame {
                                clip.setFrame(frame);
                            }
                            if clip.autoresizingMask() != backing_mask {
                                clip.setAutoresizingMask(backing_mask);
                            }
                            // Inside the wrapper the glass simply fills it.
                            let inner = NSRect::new(NSPoint::new(0.0, 0.0), frame.size);
                            if entry.glass.frame() != inner {
                                entry.glass.setFrame(inner);
                            }
                            entry.glass.setAutoresizingMask(fill_mask());
                        }
                    }
                } else {
                    if entry.glass.frame() != frame {
                        entry.glass.setFrame(frame);
                    }
                    if entry.glass.autoresizingMask() != backing_mask {
                        entry.glass.setAutoresizingMask(backing_mask);
                    }
                }
                entry.backing.setHidden(region.backing.a <= 0.0);
                entry.backing.setFillColor(&ns_color(region.backing));

                entry.glass.setHidden(region.style.is_none());
                entry.glass.setStyle(match region.style {
                    Some(GlassStyle::Clear) => NSGlassEffectViewStyle::Clear,
                    _ => NSGlassEffectViewStyle::Regular,
                });
                entry
                    .glass
                    .setCornerRadius(f64::from(f32::from(region.corner_radius)));
                entry.glass.setTintColor(
                    (region.tint.a > 0.0)
                        .then(|| ns_color(region.tint))
                        .as_deref(),
                );
                entry.last = region.clone();
                entry.shown = true;
            }
            end_no_actions();
        });
    }

    /// gpui's top-left-origin window space; the holder is flipped, so it maps
    /// straight across.
    fn ns_rect(b: Bounds<Pixels>) -> NSRect {
        let f = |p: gpui_kit::Pixels| f64::from(f32::from(p));
        NSRect::new(
            NSPoint::new(f(b.origin.x), f(b.origin.y)),
            NSSize::new(f(b.size.width), f(b.size.height)),
        )
    }

    /// A region that spans the window's full width and/or height keeps doing
    /// so while AppKit resizes the window, until gpui's next frame re-applies it.
    fn edge_mask(b: Bounds<Pixels>, window: NSSize) -> NSAutoresizingMaskOptions {
        let f = |p: gpui_kit::Pixels| f64::from(f32::from(p));
        let mut mask = NSAutoresizingMaskOptions::empty();
        if f(b.origin.x) <= 0.5 && f(b.origin.x + b.size.width) >= window.width - 0.5 {
            mask |= NSAutoresizingMaskOptions::ViewWidthSizable;
        }
        if f(b.origin.y) <= 0.5 && f(b.origin.y + b.size.height) >= window.height - 0.5 {
            mask |= NSAutoresizingMaskOptions::ViewHeightSizable;
        }
        mask
    }

    fn ns_color(c: gpui_kit::Hsla) -> Retained<NSColor> {
        let rgba = gpui_kit::Rgba::from(c);
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
    /// missing, or when `DTB_KE_NO_GLASS` is set — for testing the pre-26
    /// vibrancy tier on a Tahoe+ machine without it, the same one `install`
    /// falls back to here). `NSGlassEffectContainerView` has no typed
    /// binding in objc2-app-kit yet, so it goes through the runtime.
    fn glass_host(mtm: MainThreadMarker, content: &NSView) -> Option<GlassHost> {
        if std::env::var_os("DTB_KE_NO_GLASS").is_some() {
            debug!("native backdrop: DTB_KE_NO_GLASS set, forcing the vibrancy tier");
            return None;
        }
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
        let backing_holder = FlippedView::new(mtm, frame).into_super();
        backing_holder.setAutoresizingMask(fill_mask());
        content.addSubview_positioned_relativeTo(
            &backing_holder,
            NSWindowOrderingMode::Below,
            None,
        );
        let direct_holder = FlippedView::new(mtm, frame).into_super();
        direct_holder.setAutoresizingMask(fill_mask());
        // Just above the backing layer (bottom → top: backings, standalone
        // glass, the container, the top layer).
        content.addSubview_positioned_relativeTo(
            &direct_holder,
            NSWindowOrderingMode::Above,
            Some(&backing_holder),
        );
        let top_holder = FlippedView::new(mtm, frame).into_super();
        top_holder.setAutoresizingMask(fill_mask());
        // Above the container, unlike `direct_holder` — see its doc comment.
        content.addSubview_positioned_relativeTo(
            &top_holder,
            NSWindowOrderingMode::Above,
            Some(&container),
        );
        Some(GlassHost {
            container,
            holder,
            direct_holder,
            top_holder,
            backing_holder,
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
            Kind::Glass(host) => {
                host.container.setAppearance(appearance.as_deref());
                host.direct_holder.setAppearance(appearance.as_deref());
                host.top_holder.setAppearance(appearance.as_deref());
                host.backing_holder.setAppearance(appearance.as_deref());
            }
        }
    }
}

/// iPadOS: the glass tier only. `UIVisualEffectView`s carrying a `UIGlassEffect` sit beneath
/// gpui's Metal view (a sibling inside the `UIWindow`), one per [`GlassRegion`] exactly like the
/// macOS `NSGlassEffectView`s; before iOS 26 there is no glass class, [`available`] is false and
/// the opaque skin applies.
#[cfg(target_os = "ios")]
mod ios {
    use std::cell::RefCell;
    use std::collections::HashMap;

    use gpui_kit::{Bounds, Pixels, SharedString, Window};
    use log::{debug, warn};
    use objc2::rc::Retained;
    use objc2::runtime::AnyClass;
    use objc2::{MainThreadMarker, msg_send};
    use objc2_core_foundation::{CGPoint, CGRect, CGSize};
    use objc2_ui_kit::{
        UIColor, UICornerConfiguration, UICornerRadius, UIGlassContainerEffect, UIGlassEffect,
        UIGlassEffectStyle, UIUserInterfaceStyle, UIView, UIViewAutoresizing, UIVisualEffectView,
    };
    use raw_window_handle::{HasWindowHandle, RawWindowHandle};

    use super::Regions;
    use crate::skin::glass::{GlassRegion, GlassStyle};
    use crate::theme::ThemeMode;

    struct GlassEntry {
        backing: Retained<UIView>,
        glass: Retained<UIVisualEffectView>,
        clip: Option<Retained<UIView>>,
        last: GlassRegion,
        shown: bool,
    }

    struct Host {
        /// The window the views live in (gpui's Metal view is its child).
        content: Retained<UIView>,
        container: Retained<UIVisualEffectView>,
        direct_holder: Retained<UIView>,
        top_holder: Retained<UIView>,
        backing_holder: Retained<UIView>,
        views: HashMap<SharedString, GlassEntry>,
    }

    thread_local! {
        static STATE: RefCell<Option<Host>> = const { RefCell::new(None) };
    }

    pub fn available() -> bool {
        std::env::var_os("DTB_KE_NO_GLASS").is_none() && AnyClass::get(c"UIGlassEffect").is_some()
    }

    pub fn glass_active() -> bool {
        STATE.with(|s| s.borrow().is_some())
    }

    fn cg_rect(b: Bounds<Pixels>) -> CGRect {
        let f = |p: Pixels| f64::from(f32::from(p));
        CGRect::new(
            CGPoint::new(f(b.origin.x), f(b.origin.y)),
            CGSize::new(f(b.size.width), f(b.size.height)),
        )
    }

    fn ui_color(c: gpui_kit::Hsla) -> Retained<UIColor> {
        let rgba = gpui_kit::Rgba::from(c);
        UIColor::colorWithRed_green_blue_alpha(
            f64::from(rgba.r),
            f64::from(rgba.g),
            f64::from(rgba.b),
            f64::from(rgba.a),
        )
    }

    fn fill_mask() -> UIViewAutoresizing {
        UIViewAutoresizing::FlexibleWidth | UIViewAutoresizing::FlexibleHeight
    }

    fn plain_holder(mtm: MainThreadMarker, frame: CGRect) -> Retained<UIView> {
        let view = UIView::initWithFrame(mtm.alloc(), frame);
        view.setAutoresizingMask(fill_mask());
        view.setUserInteractionEnabled(false);
        view
    }

    fn style_of(mode: ThemeMode) -> UIUserInterfaceStyle {
        match mode {
            ThemeMode::System => UIUserInterfaceStyle::Unspecified,
            ThemeMode::Light => UIUserInterfaceStyle::Light,
            ThemeMode::Dark => UIUserInterfaceStyle::Dark,
        }
    }

    pub fn install(window: &Window, mode: ThemeMode) {
        let Some(mtm) = MainThreadMarker::new() else {
            warn!("native backdrop: not on the main thread");
            return;
        };
        remove();
        if !available() {
            debug!("native backdrop: no UIGlassEffect, staying opaque");
            return;
        }
        let Ok(handle) = HasWindowHandle::window_handle(window) else {
            warn!("native backdrop: window has no raw handle");
            return;
        };
        let RawWindowHandle::UiKit(handle) = handle.as_raw() else {
            warn!("native backdrop: unexpected raw window handle kind");
            return;
        };
        // SAFETY: gpui documents the UiKit handle as a pointer to its own (live) `UIView`; we are
        // on the main thread and only retain it.
        let Some(gpui_view) =
            (unsafe { Retained::retain(handle.ui_view.as_ptr().cast::<UIView>()) })
        else {
            return;
        };
        let Some(content) = gpui_view.superview() else {
            warn!("native backdrop: gpui view has no superview");
            return;
        };
        let frame = content.bounds();

        let container_effect = UIGlassContainerEffect::new(mtm);
        container_effect.setSpacing(0.0);
        let container =
            UIVisualEffectView::initWithEffect(mtm.alloc(), Some(&container_effect));
        container.setFrame(frame);
        container.setAutoresizingMask(fill_mask());
        container.setUserInteractionEnabled(false);

        // Bottom → top: backings, standalone glass, the merged container, the top layer — each
        // inserted just beneath gpui's view.
        let backing_holder = plain_holder(mtm, frame);
        let direct_holder = plain_holder(mtm, frame);
        let top_holder = plain_holder(mtm, frame);
        content.insertSubview_belowSubview(&backing_holder, &gpui_view);
        content.insertSubview_belowSubview(&direct_holder, &gpui_view);
        content.insertSubview_belowSubview(&container, &gpui_view);
        content.insertSubview_belowSubview(&top_holder, &gpui_view);

        let host = Host {
            content,
            container,
            direct_holder,
            top_holder,
            backing_holder,
            views: HashMap::new(),
        };
        pin_appearance(&host, mode);
        debug!("native backdrop installed (iPadOS glass tier)");
        STATE.with(|s| *s.borrow_mut() = Some(host));
    }

    pub fn remove() {
        STATE.with(|s| {
            if let Some(host) = s.borrow_mut().take() {
                host.container.removeFromSuperview();
                host.direct_holder.removeFromSuperview();
                host.top_holder.removeFromSuperview();
                host.backing_holder.removeFromSuperview();
                debug!("native backdrop removed");
            }
        });
    }

    pub fn set_mode(mode: ThemeMode) {
        STATE.with(|s| {
            if let Some(host) = s.borrow().as_ref() {
                pin_appearance(host, mode);
            }
        });
    }

    fn pin_appearance(host: &Host, mode: ThemeMode) {
        let style = style_of(mode);
        host.container.setOverrideUserInterfaceStyle(style);
        host.direct_holder.setOverrideUserInterfaceStyle(style);
        host.top_holder.setOverrideUserInterfaceStyle(style);
        host.backing_holder.setOverrideUserInterfaceStyle(style);
    }

    fn set_corner_radius(view: &UIView, radius: f64) {
        let corners = UICornerConfiguration::configurationWithUniformRadius(
            &UICornerRadius::fixedRadius(radius),
        );
        view.setCornerConfiguration(&corners);
    }

    pub fn apply_regions(regions: &Regions) {
        let Some(mtm) = MainThreadMarker::new() else {
            return;
        };
        STATE.with(|s| {
            let mut s = s.borrow_mut();
            let Some(host) = s.as_mut() else {
                return;
            };

            begin_no_actions();
            for (id, entry) in host.views.iter_mut() {
                if entry.shown && !regions.contains_key(id) {
                    entry.backing.setHidden(true);
                    entry.glass.setHidden(true);
                    if let Some(clip) = &entry.clip {
                        clip.setHidden(true);
                    }
                    entry.shown = false;
                }
            }
            for (id, region) in regions {
                if !host.views.contains_key(id) {
                    let frame = cg_rect(region.bounds);
                    let backing = UIView::initWithFrame(mtm.alloc(), frame);
                    let glass = UIVisualEffectView::initWithEffect(mtm.alloc(), None);
                    glass.setFrame(frame);
                    host.backing_holder.addSubview(&backing);
                    let clip = if region.contained {
                        host.container.contentView().addSubview(&glass);
                        None
                    } else {
                        let clip = UIView::initWithFrame(mtm.alloc(), frame);
                        clip.setClipsToBounds(true);
                        clip.addSubview(&glass);
                        if region.top {
                            host.top_holder.addSubview(&clip);
                        } else {
                            host.direct_holder.addSubview(&clip);
                        }
                        Some(clip)
                    };
                    host.views.insert(
                        id.clone(),
                        GlassEntry {
                            backing,
                            glass,
                            clip,
                            last: region.clone(),
                            shown: false,
                        },
                    );
                }
                let entry = host.views.get_mut(id).expect("just inserted");
                if entry.shown && entry.last == *region {
                    continue;
                }
                let frame = cg_rect(region.bounds);
                entry.backing.setFrame(frame);

                if let Some(clip) = &entry.clip {
                    clip.setHidden(false);
                    match &region.scroll_clip {
                        Some((_, viewport)) => {
                            clip.setFrame(cg_rect(*viewport));
                            entry.glass.setFrame(CGRect::new(
                                CGPoint::new(
                                    f64::from(f32::from(
                                        region.bounds.origin.x - viewport.origin.x,
                                    )),
                                    f64::from(f32::from(
                                        region.bounds.origin.y - viewport.origin.y,
                                    )),
                                ),
                                frame.size,
                            ));
                        }
                        None => {
                            clip.setFrame(frame);
                            entry
                                .glass
                                .setFrame(CGRect::new(CGPoint::new(0.0, 0.0), frame.size));
                        }
                    }
                } else {
                    entry.glass.setFrame(frame);
                }
                entry.backing.setHidden(region.backing.a <= 0.0);
                entry
                    .backing
                    .setBackgroundColor(Some(&ui_color(region.backing)));

                entry.glass.setHidden(region.style.is_none());
                if let Some(style) = region.style {
                    let effect = UIGlassEffect::effectWithStyle(
                        match style {
                            GlassStyle::Clear => UIGlassEffectStyle::Clear,
                            GlassStyle::Regular => UIGlassEffectStyle::Regular,
                        },
                        mtm,
                    );
                    effect.setTintColor((region.tint.a > 0.0).then(|| ui_color(region.tint)).as_deref());
                    entry.glass.setEffect(Some(&effect));
                }
                set_corner_radius(&entry.glass, f64::from(f32::from(region.corner_radius)));
                entry.last = region.clone();
                entry.shown = true;
            }
            end_no_actions();
            // The window itself takes the content colour (the `ContentBacking` region's flat
            // fill), so the sidebar's glass frosts that colour instead of the bare black window.
            if let Some(backing) = regions
                .values()
                .find(|r| r.style.is_none() && r.backing.a >= 1.0)
            {
                host.content
                    .setBackgroundColor(Some(&ui_color(backing.backing)));
            }
        });
    }

    fn begin_no_actions() {
        if let Some(class) = AnyClass::get(c"CATransaction") {
            // SAFETY: documented CATransaction class methods, balanced by `end_no_actions`.
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
}
