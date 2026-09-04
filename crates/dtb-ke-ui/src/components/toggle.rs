//! A binary on/off switch in the house style.
//!
//! The checked value is owned by the caller: [`Toggle::on_change`] reports the
//! *next* value and the caller must render it back through [`Toggle::new`].

use gpui::{
    App, ElementId, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use crate::theme::ActiveTheme;

type ChangeHandler = Box<dyn Fn(bool, &mut Window, &mut App) + 'static>;

#[derive(IntoElement)]
pub struct Toggle {
    id: ElementId,
    checked: bool,
    disabled: bool,
    on_change: Option<ChangeHandler>,
}

impl Toggle {
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

impl RenderOnce for Toggle {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.theme().color;
        let (track_w, track_h, knob) = (px(38.), px(22.), px(18.));
        let checked = self.checked;

        let track_fill = if checked { c.primary } else { c.chrome };
        let border = if checked { c.primary } else { c.border };

        let mut track = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .w(track_w)
            .h(track_h)
            .rounded(track_h)
            .border_1()
            .border_color(border)
            .bg(track_fill)
            .when(checked, |el| el.justify_end())
            .child(
                div()
                    .m(px(1.))
                    .size(knob)
                    .rounded(knob)
                    .bg(if checked {
                        c.primary_foreground
                    } else {
                        c.surface
                    })
                    .shadow_sm(),
            );

        if self.disabled {
            track = track.opacity(0.45);
        } else if let Some(handler) = self.on_change {
            track = track
                .cursor_pointer()
                .on_click(move |_, window, cx| handler(!checked, window, cx));
        }

        track
    }
}
