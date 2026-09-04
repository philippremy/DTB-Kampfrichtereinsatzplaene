//! The root wrapper for a window.
//!
//! Phase 2: establishes the themed background and full-window sizing and hosts
//! the app view. Later phases add the overlay layers (notifications, dialogs,
//! menu popovers) on top of the same wrapper.

use gpui::{AnyView, Context, IntoElement, ParentElement, Render, Styled, Window, div};

use crate::theme::ActiveTheme;

pub struct OverlayHost {
    content: AnyView,
}

impl OverlayHost {
    pub fn new(content: impl Into<AnyView>) -> Self {
        Self {
            content: content.into(),
        }
    }
}

impl Render for OverlayHost {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .size_full()
            .bg(theme.color.background)
            .text_color(theme.color.foreground)
            .child(self.content.clone())
    }
}
