//! Modal for creating a judging table: pick a discipline and a name.

use std::rc::Rc;

use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Window, div, px,
};
use gpui_kit::base::Dialog;
use gpui_kit::base::input::{Input, InputState};

use crate::components::button::{Button, ButtonTone};
use crate::detail::dialog_frame::{dialog_panel, dismiss_handler, scrim};
use crate::i18n::ActiveLocale;
use crate::model::JudgingTable;
use crate::model::roles::Discipline;
use crate::store::RoundEditor;
use crate::theme::ActiveTheme;

pub type Created = Rc<dyn Fn(&mut Window, &mut App)>;

pub struct TableWizard {
    round: Option<Entity<RoundEditor>>,
    name_input: Entity<InputState>,
    discipline: Discipline,
    open: bool,
    on_created: Created,
    focus: FocusHandle,
}

impl TableWizard {
    pub fn new(on_created: Created, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = cx.t("detail.wizard.name-placeholder");
        let name_input = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        Self {
            round: None,
            name_input,
            discipline: Discipline::CyrArtistic,
            open: false,
            on_created,
            focus: cx.focus_handle(),
        }
    }

    pub fn open_for(
        &mut self,
        round: Entity<RoundEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let default_name = cx.t_fmt(
            "detail.wizard.default-name",
            &[("n", &(round.read(cx).tables().len() + 1).to_string())],
        );
        self.round = Some(round);
        self.discipline = Discipline::CyrArtistic;
        self.name_input
            .update(cx, |input, cx| input.set_value(default_name, window, cx));
        self.open = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.round = None;
        cx.notify();
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(round) = self.round.clone() else {
            return;
        };
        let label = {
            let value = self.name_input.read(cx).value();
            let trimmed = value.trim();
            if trimmed.is_empty() {
                cx.t("detail.wizard.fallback-name").to_string()
            } else {
                trimmed.to_owned()
            }
        };
        let kind = self.discipline.blank_kind();
        round.update(cx, |round, cx| {
            round.add_table(JudgingTable { label, kind }, cx);
        });
        (self.on_created.clone())(window, cx);
        self.close(cx);
    }
}

impl Render for TableWizard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div();
        }

        let theme = cx.theme().clone();
        let c = theme.color;
        let radius = theme.skin.radius_control_px();
        let current = self.discipline;

        let disciplines =
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(Discipline::ALL.into_iter().map(|discipline| {
                    let selected = discipline == current;
                    div()
                        .id(discipline.short())
                        .px(px(10.))
                        .py(px(6.))
                        .rounded(radius)
                        .border_1()
                        .border_color(if selected { c.primary } else { c.border })
                        .bg(if selected { c.accent_soft } else { c.surface })
                        .text_color(if selected { c.primary } else { c.foreground })
                        .text_size(px(12.))
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.discipline = discipline;
                            cx.notify();
                        }))
                        .child(cx.t(discipline.label_key()))
                }));

        div().child(
            Dialog::new(cx)
                .open(true)
                .focus_handle(self.focus.clone())
                .flex()
                .items_center()
                .justify_center()
                .on_cancel(dismiss_handler(cx, |this, cx| this.close(cx)))
                .backdrop(scrim(&theme))
                .popup(
                    dialog_panel(&theme, px(380.))
                        .child(div().text_size(px(15.)).child(cx.t("detail.wizard.title")))
                        .child(super::field_label(
                            cx.t("detail.wizard.discipline-label"),
                            &c,
                        ))
                        .child(disciplines)
                        .child(super::field_label(cx.t("detail.wizard.name-label"), &c))
                        .child(
                            div()
                                .rounded(radius)
                                .border_1()
                                .border_color(c.border)
                                .bg(c.surface)
                                .px(px(8.))
                                .h(px(30.))
                                .flex()
                                .items_center()
                                .text_size(px(13.))
                                .child(Input::new(&self.name_input)),
                        )
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap(px(8.))
                                .pt(px(4.))
                                .child(
                                    Button::new(
                                        "wizard-cancel",
                                        cx.t("detail.wizard.cancel-button"),
                                    )
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
                                )
                                .child(
                                    Button::new(
                                        "wizard-create",
                                        cx.t("detail.wizard.create-button"),
                                    )
                                    .tone(ButtonTone::Primary)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.confirm(window, cx)),
                                    ),
                                ),
                        ),
                ),
        )
    }
}
