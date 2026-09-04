//! A segmented control — a small set of mutually exclusive options in a track.
//! Used for the Qualifikation | Finale switch.

use std::rc::Rc;

use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce, SharedString,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::components::focus::selection_fill;
use crate::theme::ActiveTheme;

type OnSelect = Rc<dyn Fn(usize, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Segmented {
    id: ElementId,
    options: Vec<SharedString>,
    selected: usize,
    on_select: Option<OnSelect>,
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
        }
    }

    pub fn on_select(mut self, handler: impl Fn(usize, &mut Window, &mut App) + 'static) -> Self {
        self.on_select = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Segmented {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let track_radius = theme.skin.radius_control_px();
        let inner_radius = px((theme.skin.radius_control - 2.0).max(2.0));
        let id = self.id.clone();

        div()
            .id(id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(track_radius)
            .border_1()
            .border_color(c.border)
            .bg(c.chrome)
            .children(self.options.into_iter().enumerate().map(|(i, label)| {
                let selected = i == self.selected;
                let on_select = self.on_select.clone();
                div()
                    .id(("segment", i))
                    .flex()
                    .items_center()
                    .justify_center()
                    .px(px(10.))
                    .h(px(22.))
                    .rounded(inner_radius)
                    .text_size(px(12.))
                    .text_color(if selected {
                        selection_fill(theme).1
                    } else {
                        c.muted_foreground
                    })
                    .when(selected, |el| el.bg(selection_fill(theme).0).shadow_xs())
                    .when(!selected, |el| {
                        el.cursor_pointer().hover(|el| el.text_color(c.foreground))
                    })
                    .when_some(on_select.filter(|_| !selected), |el, handler| {
                        el.on_click(move |_, window, cx| handler(i, window, cx))
                    })
                    .child(label)
            }))
    }
}
