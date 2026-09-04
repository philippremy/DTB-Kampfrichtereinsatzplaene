//! The "Ersatzkampfrichter" section for one phase — one text field per reserve
//! judge, plus a dashed "+" template to add another. Written straight back to
//! the [`RoundEditor`] on every change.

use gpui::{
    App, AppContext, Context, Entity, Focusable as _, InteractiveElement, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div, px,
};
use gpui_base::input::{InputEvent, InputState};

use crate::components::button::Button;
use crate::components::field::Field;
use crate::components::icon::Icon;
use crate::components::template_tile::TemplateTile;
use crate::store::RoundEditor;
use crate::theme::ActiveTheme;

pub struct SpareJudgesSection {
    round: Option<Entity<RoundEditor>>,
    fields: Vec<Entity<InputState>>,
    /// Normalised reserve-judge names that are also assigned in a table.
    conflict_names: Vec<String>,
    _subs: Vec<Subscription>,
}

impl SpareJudgesSection {
    pub fn new(_window: &mut Window, _cx: &mut Context<Self>) -> Self {
        Self {
            round: None,
            fields: Vec::new(),
            conflict_names: Vec::new(),
            _subs: Vec::new(),
        }
    }

    pub fn set_conflict_names(&mut self, names: Vec<String>, cx: &mut Context<Self>) {
        if self.conflict_names != names {
            self.conflict_names = names;
            cx.notify();
        }
    }

    pub fn rebind(
        &mut self,
        round: Entity<RoundEditor>,
        values: Vec<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.round = Some(round);
        self.fields = values
            .into_iter()
            .map(|value| cx.new(|cx| InputState::new(window, cx).default_value(value)))
            .collect();
        self.resubscribe(window, cx);
        cx.notify();
    }

    pub fn clear(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.round = None;
        self.fields.clear();
        self._subs.clear();
        cx.notify();
    }

    fn resubscribe(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self._subs = self
            .fields
            .iter()
            .map(|field| {
                cx.subscribe_in(field, window, |this, _, event: &InputEvent, window, cx| {
                    match event {
                        InputEvent::Change => this.write_back(cx),
                        // Enter "finishes" the field by dropping focus.
                        InputEvent::PressEnter { .. } => window.blur(cx),
                        _ => {}
                    }
                })
            })
            .collect();
    }

    /// Whether one of the reserve-judge fields currently holds focus.
    fn any_field_focused(&self, window: &Window, cx: &App) -> bool {
        self.fields
            .iter()
            .any(|field| field.read(cx).focus_handle(cx).is_focused(window))
    }

    fn add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.fields
            .push(cx.new(|cx| InputState::new(window, cx).placeholder("Name")));
        self.resubscribe(window, cx);
        cx.notify();
    }

    fn remove(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if index < self.fields.len() {
            self.fields.remove(index);
            self.resubscribe(window, cx);
            self.write_back(cx);
            cx.notify();
        }
    }

    fn write_back(&self, cx: &mut Context<Self>) {
        let Some(round) = self.round.clone() else {
            return;
        };
        let names: Vec<String> = self
            .fields
            .iter()
            .map(|field| field.read(cx).value().trim().to_string())
            .filter(|name| !name.is_empty())
            .collect();
        round.update(cx, |round, cx| round.set_spare_judges(names, cx));
    }
}

impl Render for SpareJudgesSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;

        div()
            .id("spare-judges")
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(20.))
            .border_t_1()
            .border_color(c.border)
            // Clicking outside the fields "finishes" the focused one.
            .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                if this.any_field_focused(window, cx) {
                    window.blur(cx);
                }
            }))
            .child(super::field_label("Ersatzkampfrichter*innen", &c))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .children(self.fields.iter().enumerate().map(|(index, field)| {
                        let invalid = self
                            .conflict_names
                            .contains(&field.read(cx).value().trim().to_lowercase());
                        Field::new(("spare", index), field)
                            .invalid(invalid)
                            .trailing(
                                Button::icon(("rm-spare", index), Icon::Close)
                                    .small()
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.remove(index, window, cx)
                                    })),
                            )
                    }))
                    .child(
                        TemplateTile::field("add-spare")
                            .label("Ersatzkampfrichter*in hinzufügen")
                            .on_click(cx.listener(|this, _, window, cx| this.add(window, cx))),
                    ),
            )
    }
}
