//! Glass regions — the portable half of the Liquid Glass integration.
//!
//! Views never talk to AppKit. A view that wants a glass surface drops a
//! [`region`] probe into a `relative` container; during prepaint the probe
//! records the container's window-relative bounds. A single [`end_frame`]
//! probe, painted last, hands the frame's set of regions to the platform
//! backdrop ([`super::backdrop`]), which on macOS 26+ keeps one
//! `NSGlassEffectView` per region *beneath* gpui's view. Everywhere else —
//! other platforms, macOS before 26, Reduce Transparency — the calls are
//! cheap no-ops and the view's own token fill (`material::*_fill`) is what
//! you see.
//!
//! Constraints that follow from the native views sitting *under* gpui:
//! * gpui must paint nothing opaque over a region (its fill is transparent
//!   whenever the native backdrop is active, see [`crate::material`]);
//! * the glass samples the window backdrop, not gpui content;
//! * the native frame follows gpui layout, so keep regions to static chrome
//!   (sidebar, toolbar capsules) — never per-row or per-card.

use std::cell::RefCell;
use std::collections::BTreeMap;

use gpui::{Bounds, Hsla, IntoElement, Pixels, Styled, canvas, px};

use crate::theme::{ActiveTheme, Appearance, Theme};

/// The glass variant — `NSGlassEffectViewStyle`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlassStyle {
    /// The standard, more opaque glass (navigation surfaces).
    Regular,
    /// The see-through variant that keeps the structure of what's behind it.
    Clear,
}

/// One glass surface for the current frame.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct GlassRegion {
    /// Window-relative bounds, top-left origin (gpui's space).
    pub bounds: Bounds<Pixels>,
    pub style: GlassStyle,
    pub corner_radius: Pixels,
    /// Tints the glass itself (alpha = strength; `a == 0.0` = untinted).
    pub tint: Hsla,
    /// Fill placed behind the glass (`a == 0.0` = none).
    pub backing: Hsla,
}

/// What a surface *is*; maps to a style + shape so views don't pick raw
/// glass parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlassRole {
    /// The full-height navigation sidebar (edge-to-edge, square corners).
    Sidebar,
}

impl GlassRole {
    fn style(self) -> GlassStyle {
        match self {
            GlassRole::Sidebar => GlassStyle::Regular,
        }
    }

    fn corner_radius(self) -> Pixels {
        match self {
            GlassRole::Sidebar => px(0.),
        }
    }

    /// `(tint, backing)` — both the palette's `chrome` colour at the skin's
    /// authored strengths.
    fn colors(self, theme: &Theme) -> (Hsla, Hsla) {
        let chrome = theme.color.chrome;
        let dark = theme.appearance == Appearance::Dark;
        let skin = &theme.skin;
        let tint = match (dark, skin.glass_tint_opacity_dark) {
            (true, Some(a)) => a,
            _ => skin.glass_tint_opacity,
        };
        let backing = match (dark, skin.glass_backing_opacity_dark) {
            (true, Some(a)) => a,
            _ => skin.glass_backing_opacity,
        };
        match self {
            GlassRole::Sidebar => (Hsla { a: tint, ..chrome }, Hsla { a: backing, ..chrome }),
        }
    }
}

type Frame = BTreeMap<&'static str, GlassRegion>;

thread_local! {
    /// Regions recorded so far in the frame being prepainted.
    static FRAME: RefCell<Frame> = const { RefCell::new(BTreeMap::new()) };
}

/// A zero-cost probe: place it (as a child) inside a `relative` container that
/// should be a glass surface. `id` must be unique per window.
pub fn region(id: &'static str, role: GlassRole) -> impl IntoElement {
    canvas(
        move |bounds, _window, cx| {
            if super::backdrop::glass_active() {
                let (tint, backing) = role.colors(cx.theme());
                FRAME.with(|f| {
                    f.borrow_mut().insert(
                        id,
                        GlassRegion {
                            bounds,
                            style: role.style(),
                            corner_radius: role.corner_radius(),
                            tint,
                            backing,
                        },
                    );
                });
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size_full()
}

/// Paint this **last** in the window root: its prepaint runs after every
/// [`region`] probe, so it sees the complete set for the frame, applies it to
/// the native backdrop (regions that no longer render are removed) and resets.
pub fn end_frame() -> impl IntoElement {
    canvas(
        |_, _window, _cx| {
            if super::backdrop::glass_active() {
                let frame = FRAME.with(|f| std::mem::take(&mut *f.borrow_mut()));
                super::backdrop::apply_regions(&frame);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size(px(0.))
}

/// The regions type the backdrop consumes.
pub(super) type Regions = Frame;
