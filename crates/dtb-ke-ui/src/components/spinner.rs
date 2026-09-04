//! A small indeterminate loading spinner — a rotating 3/4 arc.
//!
//! Rendered as a tinted SVG mask (like [`Icon`](super::icon::Icon)); the
//! rotation goes through `gpui::with_animation`, which snaps to a static ring
//! when `App::reduce_motion` is set.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, Hsla, IntoElement, Pixels, RenderOnce, Styled, Transformation,
    Window, percentage, px, svg,
};

use crate::theme::ActiveTheme;

/// A 270° arc centred in a 24×24 box, 3px stroke, round caps. The stroke colour
/// is irrelevant — gpui renders the SVG as a tinted alpha mask.
const ARC_SVG: &str = r#"<svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg"><path d="M12 3 a9 9 0 1 1 -6.36 2.64" fill="none" stroke="black" stroke-width="3" stroke-linecap="round"/></svg>"#;

#[derive(IntoElement)]
pub struct Spinner {
    size: Pixels,
    color: Option<Hsla>,
}

impl Spinner {
    pub fn new() -> Self {
        Self {
            size: px(14.),
            color: None,
        }
    }

    pub fn size(mut self, size: impl Into<Pixels>) -> Self {
        self.size = size.into();
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().color.muted_foreground);
        svg()
            .flex_none()
            .size(self.size)
            .text_color(color)
            .data(ARC_SVG.as_bytes())
            .with_animation(
                "spinner",
                Animation::new(Duration::from_millis(850)).repeat(),
                |el, t| el.with_transformation(Transformation::rotate(percentage(t))),
            )
    }
}
