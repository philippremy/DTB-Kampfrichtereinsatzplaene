//! A capsule that groups related toolbar controls.
//!
//! On the native glass tier (macOS 26+) the capsule is real Liquid Glass — a
//! [`glass`] region — and its own gpui fill stays transparent so the glass
//! shows through. Everywhere else it is a token-filled, bordered pill.

use gpui::{
    AnyElement, IntoElement, ParentElement, RenderOnce, Styled, Window, div,
    px,
};
use gpui::prelude::FluentBuilder;

use crate::skin::glass::{self, GlassRole};
use crate::theme::ActiveTheme;

/// Capsule height. Buttons inside are 28 px, leaving 4 px of margin.
pub const GROUP_HEIGHT: f32 = 36.0;

#[derive(IntoElement)]
pub struct ToolbarGroup {
    id: &'static str,
    prominent: bool,
    children: Vec<AnyElement>,
}

impl ToolbarGroup {
    /// `id` must be unique per window (it keys the native glass view).
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            prominent: false,
            children: Vec::new(),
        }
    }

    /// The accent-tinted capsule for the toolbar's primary action.
    pub fn prominent(mut self) -> Self {
        self.prominent = true;
        self
    }
}

impl ParentElement for ToolbarGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for ToolbarGroup {
    fn render(self, _window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let native = glass::active();
        let role = if self.prominent {
            GlassRole::CapsuleProminent
        } else {
            GlassRole::Capsule
        };

        // The glass probe must sit on an *unpadded* box (an absolute child
        // covers its parent's content box), so the padding lives on an inner
        // row and the capsule's own box carries the region + fill.
        div()
            .relative()
            .flex_none()
            .h(px(GROUP_HEIGHT))
            .rounded_full()
            .when(native, |el| el.child(glass::region(self.id, role)))
            .when(!native, |el| {
                let (fill, border) = if self.prominent {
                    (c.primary, c.primary)
                } else {
                    (c.surface, c.border)
                };
                el.bg(fill).border_1().border_color(border).shadow_xs()
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .h_full()
                    .px(px(4.))
                    .children(self.children),
            )
    }
}
