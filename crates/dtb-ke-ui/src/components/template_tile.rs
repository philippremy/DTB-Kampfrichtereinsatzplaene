//! A dashed placeholder that reads as "add one here" — a centred `+` with an
//! optional label. Used for the empty spare-judge field, the responsible-person
//! field, and the "new judging table" card.

use gpui_kit::{
    App, ClickEvent, ElementId, FontWeight, InteractiveElement, IntoElement, ParentElement,
    RenderOnce, SharedString, StatefulInteractiveElement, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::theme::ActiveTheme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Shape {
    /// One-line, control height — matches a text field.
    Field,
    /// A block, card height — matches a judging-table card.
    Card,
}

#[derive(IntoElement)]
pub struct TemplateTile {
    id: ElementId,
    shape: Shape,
    label: Option<SharedString>,
    on_click: Option<ClickHandler>,
}

impl TemplateTile {
    /// A field-sized placeholder.
    pub fn field(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            shape: Shape::Field,
            label: None,
            on_click: None,
        }
    }

    /// A card-sized placeholder.
    pub fn card(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            shape: Shape::Card,
            label: None,
            on_click: None,
        }
    }

    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for TemplateTile {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;

        let (height, radius, plus_size) = match self.shape {
            Shape::Field => (px(30.), theme.skin.radius_control_px(), px(15.)),
            Shape::Card => (px(60.), theme.skin.radius_lg_px(), px(20.)),
        };

        div()
            .id(self.id)
            .flex()
            .items_center()
            .justify_center()
            .gap(px(6.))
            .h(height)
            .w_full()
            .rounded(radius)
            .border_1()
            .border_dashed()
            .border_color(c.line_strong)
            .text_color(c.muted_foreground)
            .text_size(px(11.))
            .cursor_pointer()
            .hover(|el| {
                el.border_color(c.primary)
                    .text_color(c.primary)
                    .bg(c.accent_soft)
            })
            .when_some(self.on_click, |el, handler| {
                el.on_click(move |event, window, cx| handler(event, window, cx))
            })
            .child(
                div()
                    .text_size(plus_size)
                    .font_weight(FontWeight::LIGHT)
                    .child("+"),
            )
            .when_some(self.label, |el, label| el.child(label))
    }
}
