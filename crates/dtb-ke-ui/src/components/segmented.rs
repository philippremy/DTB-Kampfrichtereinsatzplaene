//! A segmented control — a small set of mutually exclusive options in a track.
//! Used for the Qualifikation | Finale switch.
//!
//! On the native glass tier (macOS 26+), when [`Segmented::glass`] is used, the
//! track is one untinted [`GlassRole::Track`] region (standalone, clipped to
//! the enclosing scroll pane — *not* [`GlassRole::Capsule`], which joins the
//! shared container the way the toolbar's capsules do, but has no scroll-clip
//! support at all outside static chrome, see `GlassRole::Track`'s doc
//! comment), and the selected option gets a second, accent-tinted
//! [`GlassRole::SelectionThumb`] — a genuinely separate `NSGlassEffectView`
//! layered *above* the track instead of merged into it (merging a
//! strongly-tinted region with an overlapping neutral one in the same render
//! pass blends into a flat, muddy wash rather than reading as its own
//! accent-coloured glass — see `GlassRole`'s doc comment). A click also gives
//! the whole control the same press bump [`crate::components::toolbar_group::ToolbarGroup`]'s
//! glass capsules use — a brief swell and settle, native frame only.
//! Everywhere else — no `glass` call, other platforms, pre-Tahoe, Reduce
//! Transparency — it's the plain token-filled track, and the bump is a no-op.

use std::rc::Rc;

use gpui_kit::{
    App, Bounds, ElementId, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::components::focus::selection_fill;
use crate::components::toolbar_group::Bump;
use crate::skin::glass::{self, GlassRole};
use crate::theme::ActiveTheme;

type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    on_select: Option<OnSelect>,
    glass_id: Option<&'static str>,
    viewport: Option<Bounds<Pixels>>,
}

impl Segmented {
    pub fn new(
        id: impl Into<ElementId>,
        options: impl IntoIterator<Item = impl Into<SharedString>>,
        selected: usize,
    ) -> Self {
        Self {
            id: id.into(),
            options: options.into_iter().map(Into::into).collect(),
            selected,
            on_select: None,
            glass_id: None,
            viewport: None,
        }
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }

    /// Opts this control into the native Liquid Glass tier. `id` must be
    /// unique per window (it keys the two native glass views — the track and
    /// the selected-option thumb). `viewport` is the enclosing scroll pane's
    /// current window-relative bounds (see
    /// [`crate::skin::glass::region_in_viewport`]) — `None` before it's been
    /// probed once, or when this control isn't inside a scroll pane, falls
    /// back to the plain token-filled track for that frame.
    pub fn glass(mut self, id: &'static str, viewport: Option<Bounds<Pixels>>) -> Self {
        self.glass_id = Some(id);
        self.viewport = viewport;
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let glass_id = self.glass_id;
        // The same press-bump `ToolbarGroup`'s glass capsules use: the whole
        // control swells slightly and settles back on a click, native frame
        // only (gpui's layout is untouched). No-op — `scale` stays `1.0` —
        // without `.glass(..)`. Computed before `cx.theme()` is borrowed
        // below: `Bump::new` needs `&mut App`.
        let bump = glass_id.map(|gid| Bump::new(gid, window, cx));
        let scale = bump.as_ref().map_or(1.0, |b| b.scale);

        let theme = cx.theme();
        let c = &theme.color;
        // Track = 22 px segments + 2×2 px padding + 2×1 px border. A capsule
        // skin rounds both fully; otherwise the thumb is concentric with the
        // track (its radius minus the padding).
        let track_radius = theme.skin.control_radius_px(px(28.));
        let inner_radius = match theme.skin.control_shape {
            crate::theme::ControlShape::Capsule => px(11.),
            crate::theme::ControlShape::Rounded => {
                px((theme.skin.radius_control - 2.0).max(2.0))
            }
        };
        let id = self.id.clone();
        let native = self.glass_id.is_some() && glass::active() && self.viewport.is_some();
        let viewport = self.viewport;

        // The glass probes must sit on *unpadded* boxes (an absolute child
        // covers its parent's content box, see `crate::skin::glass`'s module
        // doc comment) — so the track's own border/padding and each
        // option's own label padding move to inner divs, and the outer
        // track box + each option's own box carry the regions instead.
        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .relative()
            .h(px(28.))
            .rounded(track_radius)
            .when_some(bump.as_ref(), |el, b| {
                el.on_mouse_down(MouseButton::Left, b.on_press())
            })
            .when(native, |el| {
                el.child(glass::region_in_viewport_scaled(
                    format!("{}-track", glass_id.expect("checked by native")),
                    GlassRole::Track,
                    viewport.expect("checked by native"),
                    glass::DETAIL_SCROLL_GROUP,
                    None,
                    scale,
                ))
            })
            .when(!native, |el| el.border_1().border_color(c.border).bg(c.chrome))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    // A bit more side margin around the glass thumb on the
                    // native tier — at 2 px (matching the fallback's border +
                    // padding exactly) the thumb read as flush against the
                    // track's own rounded ends with no breathing room.
                    .px(px(if native { 4. } else { 2. }))
                    .py(px(2.))
                    .size_full()
                    .children(self.options.into_iter().enumerate().map(|(i, label)| {
                        let selected = i == self.selected;
                        let on_select = self.on_select.clone();
                        // Unpadded outer box: carries the region (so the
                        // thumb glass reaches this pill's full bounds, not
                        // inset by the label's own padding — see the module
                        // doc comment) plus colour/hover/click, all of which
                        // the padded inner row inherits.
                        div()
                            .id(("segment", i))
                            .relative()
                            .h(px(22.))
                            .rounded(inner_radius)
                            .text_color(if selected {
                                selection_fill(theme).1
                            } else {
                                c.muted_foreground
                            })
                            .when(native && selected, |el| {
                                el.child(glass::region_in_viewport_scaled(
                                    format!("{}-thumb", glass_id.expect("checked by native")),
                                    GlassRole::SelectionThumb,
                                    viewport.expect("checked by native"),
                                    glass::DETAIL_SCROLL_GROUP,
                                    None,
                                    scale,
                                ))
                            })
                            .when(!native && selected, |el| {
                                el.bg(selection_fill(theme).0).shadow_xs()
                            })
                            .when(!selected, |el| {
                                el.cursor_pointer().hover(|el| el.text_color(c.foreground))
                            })
                            .when_some(on_select.filter(|_| !selected), |el, handler| {
                                el.on_click(move |_, window, cx| handler(i, window, cx))
                            })
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .px(px(10.))
                                    .h_full()
                                    .text_size(px(12.))
                                    .child(label),
                            )
                    })),
            )
    }
}
