//! A checkbox in the house style.
//!
//! Like [`Toggle`](super::toggle::Toggle), the checked value is owned by the
//! caller: [`Checkbox::on_change`] reports the *next* value and the caller
//! re-renders it through [`Checkbox::new`].

use gpui_kit::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::components::icon::Icon;
use crate::theme::ActiveTheme;

type ChangeHandler = Box<dyn Fn(bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Checkbox {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Checkbox {
    pub fn new(id: impl Into<ElementId>, checked: bool) -> Self {
        Self {
            id: id.into(),
            checked,
            disabled: false,
            on_change: None,
        }
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_change(mut self, handler: impl Fn(bool, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Checkbox {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let checked = self.checked;

        let mut box_ = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.))
            .rounded(radius.min(px(4.)))
            .border_1()
            .border_color(if checked { c.primary } else { c.line_strong })
            .bg(if checked { c.primary } else { c.surface })
            .when(checked, |el| {
                el.child(Icon::Check.size(px(11.)).color(c.primary_foreground))
            });

        if self.disabled {
            box_ = box_.opacity(0.45);
        } else if let Some(handler) = self.on_change {
            box_ = box_
                .cursor_pointer()
                .on_click(move |_, window, cx| handler(!checked, window, cx));
        }

        box_
    }
}
