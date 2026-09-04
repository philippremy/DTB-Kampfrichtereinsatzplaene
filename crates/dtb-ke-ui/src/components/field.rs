//! A single-line text field.
//!
//! The text-editing engine is `gpui_base::input::InputState` (cursor, selection,
//! clipboard, undo, IME); this component owns only the frame — border, fill,
//! radius, focus treatment, optional leading icon and trailing element.
//!
//! The owning view creates and holds the `Entity<InputState>` (it needs a
//! `Window`), then renders `Field::new(id, &state)`.

use gpui::{
    AnyElement, App, ElementId, Entity, Focusable as _, InteractiveElement, IntoElement,
    ParentElement, RenderOnce, Styled, Window, div, prelude::FluentBuilder, px,
};
use gpui_base::input::{Input, InputState};

use crate::components::focus::{field_border_color, focus_underline};
use crate::components::icon::Icon;
use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct Field {
    id: ElementId,
    state: Entity<InputState>,
    leading: Option<Icon>,
    trailing: Option<AnyElement>,
    disabled: bool,
    invalid: bool,
    paints_background: bool,
}

impl Field {
    pub fn new(id: impl Into<ElementId>, state: &Entity<InputState>) -> Self {
        Self {
            id: id.into(),
            state: state.clone(),
            leading: None,
            trailing: None,
            disabled: false,
            invalid: false,
            paints_background: false,
        }
    }

    pub fn leading_icon(mut self, icon: Icon) -> Self {
        self.leading = Some(icon);
        self
    }

    pub fn trailing(mut self, element: impl IntoElement) -> Self {
        self.trailing = Some(element.into_any_element());
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Mark the field as holding a value the app objects to (a judge assigned to
    /// two positions) — draws a `warn`-coloured outline.
    pub fn invalid(mut self, invalid: bool) -> Self {
        self.invalid = invalid;
        self
    }

    pub fn paints_background(mut self, yes: bool) -> Self {
        self.paints_background = yes;
        self
    }
}

impl RenderOnce for Field {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let focused = !self.disabled && self.state.read(cx).focus_handle(cx).is_focused(window);

        div()
            .id(self.id)
            .relative()
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(30.))
            .px(px(8.))
            .rounded(theme.skin.radius_control_px())
            .overflow_hidden()
            .border_1()
            .border_color(if self.invalid {
                c.warn
            } else {
                field_border_color(focused, theme)
            })
            .when(self.invalid, |el| el.bg(gpui::Hsla { a: 0.10, ..c.warn }))
            .when(!self.invalid && self.paints_background, |el| {
                el.bg(c.surface)
            })
            .text_color(c.surface_foreground)
            .text_size(px(13.))
            .when(self.disabled, |el| el.opacity(0.5))
            .when_some(self.leading, |el, icon| {
                el.child(icon.size(px(15.)).color(c.muted_foreground))
            })
            .child(div().flex_1().min_w_0().child(Input::new(&self.state)))
            .when_some(self.trailing, |el, trailing| el.child(trailing))
            .when_some(focus_underline(focused, theme), |el, bar| el.child(bar))
    }
}
