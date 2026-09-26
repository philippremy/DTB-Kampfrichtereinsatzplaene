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
//! * the native frame follows gpui layout, so a region's `id` must stay
//!   stable across frames for as long as the surface it represents exists;
//! * the probe itself pins to `(top: 0, left: 0)` of its `relative` container
//!   explicitly (not just `.absolute().size_full()`) — gpui falls back to an
//!   element's ordinary *flow* position on any axis with no inset set at all,
//!   so an unpinned probe only happens to land at the container's origin
//!   when it's the first child; placed after normal-flow siblings (as
//!   [`region_in_viewport`] typically is, alongside a pane's real scrolled
//!   content) it silently reports the wrong bounds instead.
//!
//! Static chrome (sidebar, toolbar capsules) uses [`region`]/[`region_scaled`]
//! with a fixed `&'static str` id. Content inside a scrollable pane (the
//! detail view's judging-table / spare-judges / remarks cards) uses
//! [`region_in_viewport`] instead: gpui keeps laying out and prepainting
//! scrolled-out content as usual (nothing here is virtualised), so a card's
//! probe still fires every frame with its true — possibly off-screen —
//! window-relative bounds; what changes is that its native views are clipped
//! to the enclosing pane's own visible bounds (the `viewport` argument)
//! rather than floating freely, so a card scrolled under the toolbar band or
//! past the bottom edge is actually hidden instead of drawing over other
//! chrome. See [`GlassRegion::scroll_clip`] and [`super::backdrop`]'s
//! handling of it.

use std::sync::Mutex;
use std::collections::BTreeMap;

use gpui_kit::{App, Bounds, Hsla, IntoElement, Pixels, SharedString, Styled, Window, canvas, px};

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
#[derive(Clone, PartialEq, Debug)]
pub struct GlassRegion {
    /// Window-relative bounds, top-left origin (gpui's space).
    pub bounds: Bounds<Pixels>,
    /// `None` = no glass, only the [`Self::backing`] fill (the toolbar band).
    pub style: Option<GlassStyle>,
    pub corner_radius: Pixels,
    /// Tints the glass itself (alpha = strength; `a == 0.0` = untinted).
    pub tint: Hsla,
    /// Fill placed behind the glass (`a == 0.0` = none).
    pub backing: Hsla,
    /// Whether the glass joins the shared container (nearby shapes merge and
    /// render together) or stands alone. Large surfaces stand alone: merged
    /// into the container they enlarged the capsules' shadows.
    pub contained: bool,
    /// A standalone (`contained == false`) region that must render *above*
    /// the merged container rather than below it — see
    /// [`GlassRole::SelectionThumb`]. Meaningless when `contained` is `true`.
    pub top: bool,
    /// This region lives inside a scrollable pane: `(group id, the pane's
    /// current window-relative visible bounds)`. Every region sharing a
    /// group id is clipped to that same rect instead of its own frame — see
    /// [`region_in_viewport`] and the module doc comment. `None` for the
    /// ordinary, freely-floating chrome regions.
    pub scroll_clip: Option<(SharedString, Bounds<Pixels>)>,
}

/// What a surface *is*; maps to a style + shape so views don't pick raw
/// glass parameters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum GlassRole {
    /// The full-height navigation sidebar (edge-to-edge, square corners).
    Sidebar,
    /// The whole content column (toolbar band + detail): no glass of its own,
    /// just the opaque content colour underneath. The band's gpui pixels must
    /// stay transparent for the capsules to show, so its flat colour has to
    /// come from below — and it spans the detail area too, so the sidebar's
    /// glass, which bends light at its edge, always samples that one colour
    /// instead of the bare desktop below the band.
    ContentBacking,
    /// The sidebar column while the window is fullscreen: no glass, just the opaque chrome
    /// colour — painted natively (beneath the toolbar capsules' glass) rather than by gpui,
    /// whose opaque fill would cover them.
    SidebarBacking,
    /// A group of toolbar controls in one capsule.
    Capsule,
    /// The one primary toolbar action — accent-tinted capsule.
    CapsuleProminent,
    /// A detail-view card (judging table, spare judges, remarks) — a
    /// standalone floating glass panel, always used with
    /// [`region_in_viewport`] so it's clipped to its scroll pane.
    Card,
    /// A capsule-shaped standalone surface confined to a scroll pane's
    /// viewport — e.g. a segmented control's track. Unlike [`GlassRole::Capsule`]
    /// (which joins the shared container so *nearby toolbar capsule groups*
    /// can visually merge) this never needs to merge with anything, and
    /// [`super::backdrop::apply_regions`]'s scroll-clip handling only exists
    /// on the standalone code path — a `Capsule` region fed through
    /// [`region_in_viewport`] would render at its full, unclipped bounds
    /// regardless of the pane's viewport.
    Track,
    /// A small accent-tinted badge layered *above* another glass surface —
    /// e.g. a segmented control's selection thumb, sized and positioned over
    /// the active option. Unlike [`GlassRole::CapsuleProminent`] this never
    /// joins the shared container: a tinted region merged with an
    /// overlapping neutral one in the same render pass blends into a flat,
    /// muddy wash rather than reading as its own accent-coloured glass — so
    /// this gets a genuinely separate `NSGlassEffectView`, stacked on its own
    /// top layer above the merged container (see [`super::backdrop`]'s
    /// `top_holder`).
    SelectionThumb,
    /// A colour-tinted overlay on top of a [`GlassRole::Card`] — a selected
    /// or conflicted judging-table card's accent panel. Always paired with
    /// [`region_in_viewport`]'s `tint_override` (there's no single fixed
    /// accent colour to default to — `warn` for a conflict, `primary` for a
    /// selection) and rendered a few points smaller than the card underneath
    /// it, so a sliver of the plain card glass stays visible as a frame —
    /// the same construction as [`GlassRole::SelectionThumb`] over
    /// [`GlassRole::Track`], and for the same reason: merged with the (also
    /// untinted) card beneath it, a strong tint would blend into a flat wash
    /// instead of reading as its own accent-coloured glass.
    CardOverlay,
}

impl GlassRole {
    fn style(self) -> Option<GlassStyle> {
        match self {
            GlassRole::ContentBacking | GlassRole::SidebarBacking => None,
            GlassRole::Sidebar
            | GlassRole::Capsule
            | GlassRole::CapsuleProminent
            | GlassRole::Card
            | GlassRole::SelectionThumb
            | GlassRole::Track
            | GlassRole::CardOverlay => Some(GlassStyle::Regular),
        }
    }

    fn contained(self) -> bool {
        !matches!(
            self,
            GlassRole::Sidebar
                | GlassRole::SidebarBacking
                | GlassRole::Card
                | GlassRole::SelectionThumb
                | GlassRole::Track
                | GlassRole::CardOverlay
        )
    }

    /// A standalone region that must render above the merged container
    /// instead of below it — see [`GlassRole::SelectionThumb`].
    fn top(self) -> bool {
        matches!(self, GlassRole::SelectionThumb | GlassRole::CardOverlay)
    }

    /// Capsules are fully rounded, whatever size they lay out to; a card
    /// (and its overlay) keeps the skin's ordinary large-radius corner.
    fn corner_radius(self, bounds: Bounds<Pixels>, theme: &Theme) -> Pixels {
        match self {
            GlassRole::Sidebar | GlassRole::ContentBacking | GlassRole::SidebarBacking => px(0.),
            GlassRole::Capsule
            | GlassRole::CapsuleProminent
            | GlassRole::SelectionThumb
            | GlassRole::Track => bounds.size.width.min(bounds.size.height) / 2.0,
            GlassRole::Card => theme.skin.radius_lg_px(),
            GlassRole::CardOverlay => theme.skin.radius_lg_px() - px(6.),
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
        let none = Hsla { a: 0.0, ..chrome };
        match self {
            GlassRole::Sidebar => (
                Hsla { a: tint, ..chrome },
                Hsla {
                    a: backing,
                    ..chrome
                },
            ),
            GlassRole::SidebarBacking => (
                none,
                Hsla {
                    a: 1.0,
                    ..theme.color.chrome
                },
            ),
            GlassRole::ContentBacking => (
                none,
                Hsla {
                    a: 1.0,
                    ..theme.color.background
                },
            ),
            // The card's own glass material carries enough visual weight on
            // its own — no extra tint/backing, same call as the plain
            // toolbar capsule.
            GlassRole::Capsule | GlassRole::Card | GlassRole::Track | GlassRole::CardOverlay => {
                (none, none)
            }
            GlassRole::CapsuleProminent | GlassRole::SelectionThumb => (
                Hsla {
                    a: 0.9,
                    ..theme.color.primary
                },
                none,
            ),
        }
    }
}

type Frame = BTreeMap<SharedString, GlassRegion>;

/// Regions recorded so far in the frame being prepainted. Process-global behind a `Mutex`, not
/// thread-local: it must not depend on prepaint and the end-of-frame apply running on the same thread.
static FRAME: Mutex<Frame> = Mutex::new(BTreeMap::new());

fn frame() -> std::sync::MutexGuard<'static, Frame> {
    // The map is only inserted into / taken whole, so a poisoned lock holds no half-done state.
    FRAME.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Publishes `region` into this frame's registry. It goes through
/// [`Window::replayable_effect`] so that a cached view that is reused from the previous frame
/// (skipping its prepaint) still contributes its regions — otherwise they would drop out of
/// [`end_frame`]'s set and the native glass would flicker or vanish.
fn record(id: SharedString, region: GlassRegion, window: &mut Window, cx: &mut App) {
    window.replayable_effect(cx, move |_, _| {
        frame().insert(id.clone(), region.clone());
    });
}

/// A zero-cost probe: place it (as a child) inside a `relative` container that
/// should be a glass surface. `id` must be unique per window and, for a
/// surface that comes and goes across frames, stable for as long as that
/// surface logically exists (its identity, not its current visibility).
pub fn region(id: impl Into<SharedString>, role: GlassRole) -> impl IntoElement {
    region_scaled(id, role, 1.0)
}

/// `bounds` grown by `scale` about its own centre — the press bump, shared
/// by [`region_scaled`] and [`region_in_viewport_scaled`].
fn scale_about_center(bounds: Bounds<Pixels>, scale: f32) -> Bounds<Pixels> {
    if scale == 1.0 {
        return bounds;
    }
    let (w, h) = (bounds.size.width * scale, bounds.size.height * scale);
    Bounds {
        origin: gpui_kit::point(
            bounds.origin.x - (w - bounds.size.width) / 2.0,
            bounds.origin.y - (h - bounds.size.height) / 2.0,
        ),
        size: gpui_kit::size(w, h),
    }
}

/// [`region`] with the glass grown by `scale` about its centre — the press
/// bump. Only the native frame changes; gpui's layout is untouched.
pub fn region_scaled(id: impl Into<SharedString>, role: GlassRole, scale: f32) -> impl IntoElement {
    let id = id.into();
    canvas(
        move |bounds, window, cx| {
            if super::backdrop::glass_active() {
                let bounds = scale_about_center(bounds, scale);
                let (tint, backing) = role.colors(cx.theme());
                record(
                    id,
                    GlassRegion {
                        bounds,
                        style: role.style(),
                        corner_radius: role.corner_radius(bounds, cx.theme()),
                        tint,
                        backing,
                        contained: role.contained(),
                        top: role.top(),
                        scroll_clip: None,
                    },
                    window,
                    cx,
                );
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// [`region`] for a surface inside a scrollable pane. `viewport` is that
/// pane's own current window-relative bounds (its visible clip rect); every
/// region sharing `group` is clipped to it, so scrolled-out content is
/// actually hidden rather than drawn over neighbouring chrome. See the
/// module doc comment.
///
/// `tint_override` replaces `role`'s own resting tint for this one instance
/// — `None` keeps the role default (untinted, for a plain [`GlassRole::Card`]).
/// A card that needs to read as selected or conflicted passes `Some(..)`
/// instead of drawing an opaque gpui fill over the glass, which would defeat
/// the point of it being glass.
///
/// gpui keeps prepainting scrolled-out content as usual here (nothing is
/// virtualised), so this probe fires every frame with the card's true
/// bounds — including while it's off-screen — exactly like [`region`]; the
/// only difference is the clip rect handed to the native backdrop.
pub fn region_in_viewport(
    id: impl Into<SharedString>,
    role: GlassRole,
    viewport: Bounds<Pixels>,
    group: impl Into<SharedString>,
    tint_override: Option<Hsla>,
) -> impl IntoElement {
    region_in_viewport_scaled(id, role, viewport, group, tint_override, 1.0)
}

/// [`region_in_viewport`] with the glass grown by `scale` about its centre —
/// the press bump (see [`region_scaled`]), for a region that also needs
/// scroll-pane clipping.
pub fn region_in_viewport_scaled(
    id: impl Into<SharedString>,
    role: GlassRole,
    viewport: Bounds<Pixels>,
    group: impl Into<SharedString>,
    tint_override: Option<Hsla>,
    scale: f32,
) -> impl IntoElement {
    let id = id.into();
    let group = group.into();
    canvas(
        move |bounds, window, cx| {
            if super::backdrop::glass_active() {
                let bounds = scale_about_center(bounds, scale);
                let (role_tint, backing) = role.colors(cx.theme());
                let tint = tint_override.unwrap_or(role_tint);
                record(
                    id,
                    GlassRegion {
                        bounds,
                        style: role.style(),
                        corner_radius: role.corner_radius(bounds, cx.theme()),
                        tint,
                        backing,
                        contained: role.contained(),
                        top: role.top(),
                        scroll_clip: Some((group, viewport)),
                    },
                    window,
                    cx,
                );
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

/// The shared [`region_in_viewport`] group id for the detail view's own
/// scroll pane — judging-table, spare-judges and remarks cards all clip to
/// it.
pub const DETAIL_SCROLL_GROUP: &str = "detail-scroll";

/// Paint this **last** in the window root: its prepaint runs after every
/// [`region`] probe, so it sees the complete set for the frame, applies it to
/// the native backdrop (regions that no longer render are removed) and resets.
pub fn end_frame() -> impl IntoElement {
    canvas(
        |_, _window, _cx| {
            if super::backdrop::glass_active() {
                let regions = std::mem::take(&mut *frame());
                super::backdrop::apply_regions(&regions);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .size(px(0.))
}

/// The regions type the backdrop consumes.
pub(super) type Regions = Frame;

/// Whether glass regions are being drawn natively right now (macOS 26+ and
/// not "Reduce Transparency"). Components use it to choose between a
/// transparent surface (the native glass shows through) and their token fill.
pub fn active() -> bool {
    super::backdrop::glass_active()
}

/// The macOS skin, but the glass tier *isn't* in effect right now (pre-Tahoe,
/// `DTB_KE_NO_GLASS`, or Reduce Transparency) — as opposed to Windows/Linux,
/// which have their own distinct non-glass look that a macOS-specific
/// fallback treatment must not touch. `false` off macOS and whenever
/// [`active`] is `true`.
pub fn mac_fallback() -> bool {
    super::backdrop::available() && !active()
}
