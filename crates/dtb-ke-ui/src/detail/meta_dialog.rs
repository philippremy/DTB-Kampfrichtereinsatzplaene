//! Modal for the competition's metadata: name, date, location, responsible
//! persons, meeting times. Commits everything to the `MetadataEditor` on save.

use std::cell::Cell;
use std::rc::Rc;

use chrono::{NaiveDate, NaiveTime};
use dtb_ke_types::{MeetingTimeDTO, OrganizationDTO};
use gpui::{
    App, AppContext, Bounds, Context, Entity, FocusHandle, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, PromptLevel, Render, StatefulInteractiveElement, Styled,
    Window, anchored, canvas, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui_base::Dialog;
use gpui_base::input::InputState;

use crate::components::button::{Button, ButtonTone};
use crate::components::field::Field;
use crate::components::icon::Icon;
use crate::components::template_tile::TemplateTile;
use crate::detail::dialog_frame::{dialog_panel, dismiss_handler, scrim};
use crate::i18n::ActiveLocale;
use crate::model::CompetitionMeta;
use crate::store::{MetadataEditor, default_new_meta};
use crate::theme::ActiveTheme;

const DATE_FMT: &str = "%d.%m.%Y";
const TIME_FMT: &str = "%H:%M";

/// Invoked (after the dialog's own confirm) to delete the whole competition.
pub type DeleteRequest = Rc<dyn Fn(&mut Window, &mut App)>;
/// Invoked on save in "new competition" mode with the validated metadata.
pub type CreateRequest = Rc<dyn Fn(CompetitionMeta, &mut Window, &mut App)>;

pub struct MetaDialog {
    target: Option<Entity<MetadataEditor>>,
    /// `true` while the dialog is collecting the metadata for a *new*
    /// competition (nothing persisted until Save; date + name required).
    creating: bool,
    on_delete: DeleteRequest,
    on_create: CreateRequest,
    open: bool,
    focus: FocusHandle,

    name: Entity<InputState>,
    location: Entity<InputState>,
    date: Entity<InputState>,
    organization: OrganizationDTO,
    org_open: bool,
    /// Absolute bounds of the organisation trigger, captured during prepaint (via
    /// a `canvas` probe — not an entity update, which would dead-lock) so the
    /// deferred popover can anchor to it.
    org_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    split_meeting: bool,
    unified_time: Entity<InputState>,
    quali_time: Entity<InputState>,
    finale_time: Entity<InputState>,
    persons: Vec<Entity<InputState>>,
}

impl MetaDialog {
    pub fn new(
        on_delete: DeleteRequest,
        on_create: CreateRequest,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let field = |placeholder: &str, window: &mut Window, cx: &mut Context<Self>| {
            let placeholder = placeholder.to_owned();
            cx.new(|cx| InputState::new(window, cx).placeholder(placeholder))
        };
        Self {
            target: None,
            creating: false,
            on_delete,
            on_create,
            open: false,
            focus: cx.focus_handle(),
            name: field(&cx.t("detail.meta.name-placeholder"), window, cx),
            location: field(&cx.t("detail.meta.location-placeholder"), window, cx),
            date: field("TT.MM.JJJJ", window, cx),
            organization: OrganizationDTO::DTB,
            org_open: false,
            org_bounds: Rc::new(Cell::new(None)),
            split_meeting: false,
            unified_time: field("HH:MM", window, cx),
            quali_time: field("HH:MM", window, cx),
            finale_time: field("HH:MM", window, cx),
            persons: Vec::new(),
        }
    }

    /// Open to edit an existing competition's metadata.
    pub fn open_for(
        &mut self,
        target: Entity<MetadataEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let meta = target.read(cx).meta().clone();
        self.target = Some(target);
        self.creating = false;
        self.seed(&meta, window, cx);
        self.open = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    /// Open to collect the metadata for a brand-new competition. Nothing is
    /// persisted until Save (and only if date + name are set).
    pub fn open_for_new(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.target = None;
        self.creating = true;
        self.seed(&default_new_meta(), window, cx);
        self.open = true;
        self.focus.focus(window, cx);
        cx.notify();
    }

    fn seed(&mut self, meta: &CompetitionMeta, window: &mut Window, cx: &mut Context<Self>) {
        self.org_open = false;
        self.organization = meta.organization;

        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        set(&self.name, meta.name.clone(), window, cx);
        set(&self.location, meta.location.clone(), window, cx);
        set(
            &self.date,
            meta.date.format(DATE_FMT).to_string(),
            window,
            cx,
        );

        match &meta.meeting_times {
            MeetingTimeDTO::Unified(t) => {
                self.split_meeting = false;
                set(
                    &self.unified_time,
                    t.format(TIME_FMT).to_string(),
                    window,
                    cx,
                );
            }
            MeetingTimeDTO::Split {
                qualification,
                finale,
            } => {
                self.split_meeting = true;
                set(
                    &self.quali_time,
                    qualification.format(TIME_FMT).to_string(),
                    window,
                    cx,
                );
                set(
                    &self.finale_time,
                    finale.format(TIME_FMT).to_string(),
                    window,
                    cx,
                );
            }
        }

        self.persons = meta
            .responsible_persons
            .iter()
            .map(|name| {
                let name = name.clone();
                cx.new(|cx| InputState::new(window, cx).default_value(name))
            })
            .collect();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.target = None;
        self.creating = false;
        self.org_open = false;
        cx.notify();
    }

    fn set_organization(&mut self, org: OrganizationDTO, cx: &mut Context<Self>) {
        self.organization = org;
        self.org_open = false;
        cx.notify();
    }

    /// The parsed name / date, and whether Save is allowed (always in edit
    /// mode; needs a name + a valid date in create mode).
    fn parsed_name(&self, cx: &Context<Self>) -> String {
        self.name.read(cx).value().trim().to_string()
    }

    fn parsed_date(&self, cx: &Context<Self>) -> Option<NaiveDate> {
        NaiveDate::parse_from_str(self.date.read(cx).value().trim(), DATE_FMT).ok()
    }

    fn can_save(&self, cx: &Context<Self>) -> bool {
        !self.creating || (!self.parsed_name(cx).is_empty() && self.parsed_date(cx).is_some())
    }

    /// Ask a second time, then hand off to the delete callback and close.
    fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let name = self.name.read(cx).value().trim().to_string();
        let detail = if name.is_empty() {
            cx.t("detail.meta.delete-detail-unnamed").to_string()
        } else {
            cx.t_fmt("detail.meta.delete-detail", &[("name", &name)])
        };
        let answer = window.prompt(
            PromptLevel::Critical,
            &cx.t("detail.meta.delete-confirm"),
            Some(&detail),
            &[
                gpui::PromptButton::new(cx.t("detail.meta.delete-button")),
                gpui::PromptButton::new(cx.t("detail.meta.delete-cancel-button")),
            ],
            cx,
        );
        let on_delete = self.on_delete.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            cx.update(|window, cx| on_delete(window, cx)).ok();
            this.update(cx, |this, cx| this.close(cx)).ok();
        })
        .detach();
    }

    fn add_person(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let placeholder = cx.t("detail.meta.person-placeholder");
        self.persons
            .push(cx.new(|cx| InputState::new(window, cx).placeholder(placeholder)));
        cx.notify();
    }

    fn remove_person(&mut self, index: usize, cx: &mut Context<Self>) {
        if index < self.persons.len() {
            self.persons.remove(index);
            cx.notify();
        }
    }

    fn confirm(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_save(cx) {
            return;
        }

        let name = self.parsed_name(cx);
        let location = self.location.read(cx).value().trim().to_string();
        let date = self.parsed_date(cx);
        let meeting = self.parsed_meeting(cx);
        let persons: Vec<String> = self
            .persons
            .iter()
            .map(|input| input.read(cx).value().trim().to_string())
            .filter(|person| !person.is_empty())
            .collect();

        if self.creating {
            let meta = CompetitionMeta {
                name,
                organization: self.organization,
                location,
                date: date.unwrap_or_else(|| default_new_meta().date),
                meeting_times: meeting.unwrap_or_else(|| default_new_meta().meeting_times),
                responsible_persons: persons,
            };
            let on_create = self.on_create.clone();
            self.close(cx);
            on_create(meta, window, cx);
            return;
        }

        let Some(target) = self.target.clone() else {
            return;
        };
        let organization = self.organization;
        target.update(cx, |meta, cx| {
            meta.set_name(name, cx);
            meta.set_location(location, cx);
            meta.set_organization(organization, cx);
            if let Some(date) = date {
                meta.set_date(date, cx);
            }
            if let Some(meeting) = meeting {
                meta.set_meeting_times(meeting, cx);
            }
            *meta.responsible_persons_mut(cx) = persons;
        });

        self.close(cx);
    }

    fn org_selector(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.org_open;

        // A real popover: painted via `deferred` at the trigger's *absolute*
        // position (captured during prepaint) — it floats above the dialog
        // panel and never affects its height.
        let list = open.then(|| self.org_bounds.get()).flatten().map(|b| {
            let anchor = point(b.origin.x, b.origin.y + b.size.height + px(3.));

            let mut col = div()
                .id("org-list")
                .w(b.size.width)
                .max_h(px(260.))
                .flex()
                .flex_col()
                .py(px(2.))
                .rounded(radius)
                .border_1()
                .border_color(c.border)
                .bg(c.surface)
                .shadow_lg()
                .overflow_y_scroll()
                .occlude();
            for (i, org) in OrganizationDTO::all().into_iter().enumerate() {
                let selected = org == self.organization;
                col = col.child(
                    div()
                        .id(("org-opt", i))
                        .px(px(8.))
                        .py(px(4.))
                        .text_size(px(12.5))
                        .cursor_pointer()
                        .when(selected, |el| el.text_color(c.primary))
                        .hover(|el| el.bg(c.accent_soft))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _w, cx| this.set_organization(org, cx)),
                        )
                        .child(org.to_string()),
                );
            }
            // `paint_deferred_draws` paints deferred layers in a flat priority
            // sort, and `gpui_base::Dialog` sits at `10 + layer`; `POPUP_PRIORITY`
            // (100) is gpui-base's convention for "above a dialog". Without this
            // the dialog panel paints over the popover and it's invisible.
            deferred(anchored().position(anchor).snap_to_window().child(col))
                .with_priority(gpui_base::POPUP_PRIORITY)
        });

        let capture = self.org_bounds.clone();

        div()
            .id("org-select")
            .relative()
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                if this.org_open {
                    this.org_open = false;
                    cx.notify();
                }
            }))
            // Capture the *outer* bounds of the control (no padding/border of its
            // own) so the popover aligns with the trigger's visible left edge.
            .child(
                canvas(
                    move |bounds, window, _cx| {
                        if capture.get() != Some(bounds) {
                            capture.set(Some(bounds));
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .id("org-select-trigger")
                    .flex()
                    .items_center()
                    .justify_between()
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(radius)
                    .border_1()
                    .border_color(if open { c.primary } else { c.border })
                    .bg(c.surface)
                    .text_size(px(13.))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.org_open = !this.org_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(self.organization.to_string()),
                    )
                    .child(Icon::ChevronDown.size(px(14.)).color(c.muted_foreground)),
            )
            .children(list)
    }

    fn parsed_meeting(&self, cx: &Context<Self>) -> Option<MeetingTimeDTO> {
        let parse = |input: &Entity<InputState>| {
            NaiveTime::parse_from_str(input.read(cx).value().trim(), TIME_FMT).ok()
        };
        if self.split_meeting {
            Some(MeetingTimeDTO::Split {
                qualification: parse(&self.quali_time)?,
                finale: parse(&self.finale_time)?,
            })
        } else {
            Some(MeetingTimeDTO::Unified(parse(&self.unified_time)?))
        }
    }
}

impl Render for MetaDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div();
        }

        let theme = cx.theme().clone();
        let c = theme.color;
        let radius = theme.skin.radius_control_px();
        let creating = self.creating;
        let can_save = self.can_save(cx);
        let title = if creating {
            cx.t("detail.meta.title-new")
        } else {
            cx.t("detail.meta.title-edit")
        };

        let toggle = |id: &'static str,
                      label: gpui::SharedString,
                      split: bool,
                      active: bool,
                      cx: &mut Context<Self>| {
            div()
                .id(id)
                .px(px(10.))
                .py(px(5.))
                .rounded(radius)
                .border_1()
                .border_color(if active { c.primary } else { c.border })
                .bg(if active { c.accent_soft } else { c.surface })
                .text_color(if active { c.primary } else { c.foreground })
                .text_size(px(12.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _w, cx| {
                    this.split_meeting = split;
                    cx.notify();
                }))
                .child(label)
        };

        let meeting_row = if self.split_meeting {
            div()
                .flex()
                .gap(px(8.))
                .child(labelled_time(
                    cx.t("detail.meta.meeting-qualification"),
                    &self.quali_time,
                    &c,
                    radius,
                ))
                .child(labelled_time(
                    cx.t("detail.meta.meeting-finale"),
                    &self.finale_time,
                    &c,
                    radius,
                ))
        } else {
            div().child(labelled_time(
                cx.t("detail.meta.meeting-unified"),
                &self.unified_time,
                &c,
                radius,
            ))
        };

        let person_rows = div().flex().flex_col().gap(px(6.)).children(
            self.persons
                .iter()
                .enumerate()
                .map(|(index, input)| {
                    Field::new(("person", index), input).trailing(
                        Button::icon(("rm-person", index), Icon::Close)
                            .small()
                            .on_click(
                                cx.listener(move |this, _, _w, cx| this.remove_person(index, cx)),
                            ),
                    )
                })
                .collect::<Vec<_>>(),
        );

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
                    dialog_panel(&theme, px(440.))
                        .child(div().text_size(px(15.)).child(title))
                        .child(super::field_label(cx.t("detail.meta.name-label"), &c))
                        .child(
                            Field::new("meta-name", &self.name)
                                .invalid(creating && self.parsed_name(cx).is_empty()),
                        )
                        .child(super::field_label(
                            cx.t("detail.meta.organization-label"),
                            &c,
                        ))
                        .child(self.org_selector(cx))
                        .child(super::field_label(cx.t("detail.meta.date-label"), &c))
                        .child(
                            Field::new("meta-date", &self.date)
                                .invalid(creating && self.parsed_date(cx).is_none()),
                        )
                        .child(super::field_label(cx.t("detail.meta.location-label"), &c))
                        .child(Field::new("meta-location", &self.location))
                        .child(super::field_label(
                            cx.t("detail.meta.meeting-time-label"),
                            &c,
                        ))
                        .child(
                            div()
                                .flex()
                                .gap(px(6.))
                                .child(toggle(
                                    "meeting-unified-toggle",
                                    cx.t("detail.meta.meeting-unified-toggle"),
                                    false,
                                    !self.split_meeting,
                                    cx,
                                ))
                                .child(toggle(
                                    "meeting-split-toggle",
                                    cx.t("detail.meta.meeting-split-toggle"),
                                    true,
                                    self.split_meeting,
                                    cx,
                                )),
                        )
                        .child(meeting_row)
                        .child(super::field_label(cx.t("detail.meta.persons-label"), &c))
                        .child(person_rows)
                        .child(
                            TemplateTile::field("add-person")
                                .label(cx.t("detail.meta.add-person"))
                                .on_click(
                                    cx.listener(|this, _, window, cx| this.add_person(window, cx)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(8.))
                                .pt(px(6.))
                                .when(!creating, |el| {
                                    el.child(
                                        Button::new(
                                            "meta-delete",
                                            cx.t("detail.meta.delete-competition-button"),
                                        )
                                        .tone(ButtonTone::Danger)
                                        .on_click(
                                            cx.listener(|this, _, window, cx| {
                                                this.request_delete(window, cx)
                                            }),
                                        ),
                                    )
                                })
                                .child(div().flex_1())
                                .child(
                                    Button::new("meta-cancel", cx.t("detail.meta.cancel-button"))
                                        .tone(ButtonTone::Ghost)
                                        .on_click(cx.listener(|this, _, _w, cx| this.close(cx))),
                                )
                                .child(
                                    Button::new("meta-save", cx.t("detail.meta.save-button"))
                                        .tone(ButtonTone::Primary)
                                        .disabled(!can_save)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm(window, cx)
                                        })),
                                ),
                        ),
                ),
        )
    }
}

fn labelled_time(
    label: impl Into<gpui::SharedString>,
    input: &Entity<InputState>,
    c: &crate::theme::PaletteColors,
    radius: gpui::Pixels,
) -> gpui::Div {
    div()
        .flex()
        .flex_col()
        .gap(px(3.))
        .child(super::field_label(label, c))
        .child(
            div()
                .w(px(90.))
                .h(px(30.))
                .px(px(8.))
                .flex()
                .items_center()
                .rounded(radius)
                .border_1()
                .border_color(c.border)
                .bg(c.surface)
                .text_size(px(13.))
                .child(gpui_base::input::Input::new(input)),
        )
}
