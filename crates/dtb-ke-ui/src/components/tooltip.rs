//! A small text tooltip for icon-only controls.

use gpui_kit::{
    AnyView, App, AppContext, IntoElement, ParentElement, Render, SharedString, Styled, Window, div,
    px,
};

use crate::theme::ActiveTheme;

/// The tooltip view itself — a themed, single-line label.
pub struct TextTooltip(SharedString);

impl Render for TextTooltip {
    fn render(&mut self, _window: &mut Window, cx: &mut gpui_kit::Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .px(px(8.))
            .py(px(4.))
            .rounded(theme.skin.radius_px())
            .border_1()
            .border_color(theme.color.border)
            .bg(theme.color.surface)
            .text_color(theme.color.surface_foreground)
            .text_size(px(11.5))
            .shadow_sm()
            .child(self.0.clone())
    }
}

/// A builder for `.tooltip(…)` on any stateful element.
pub fn text_tooltip(label: SharedString) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    move |_window, cx| cx.new(|_| TextTooltip(label.clone())).into()
}
