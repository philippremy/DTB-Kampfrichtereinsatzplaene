//! Modal for the competition's metadata: name, date, location, responsible
//! persons, meeting times. Commits everything to the `MetadataEditor` on save.

use std::rc::Rc;

use chrono::{NaiveDate, NaiveTime, Weekday};
use dtb_ke_types::{MeetingTimeDTO, OrganizationDTO};
use gpui_kit::{
    App, AppContext, Context, Entity, FocusHandle, InteractiveElement, IntoElement, MouseButton,
    ParentElement, PromptLevel, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, prelude::FluentBuilder, px,
};
use gpui_kit::base::Dialog;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::base::{Calendar, CalendarEvent, CalendarItemKind, CalendarState, DatePicker};

use crate::components::button::{Button, ButtonTone};
use crate::components::field::Field;
use crate::components::icon::Icon;
use crate::components::popover::PopoverAnchor;
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
    organization: OrganizationDTO,
    org_open: bool,
    /// Absolute bounds of the organisation trigger, captured during prepaint (via
    /// a `canvas` probe — not an entity update, which would dead-lock) so the
    /// deferred popover can anchor to it.
    org_bounds: PopoverAnchor,
    /// Focus handle for the date-picker trigger — `gpui_kit::base::DatePicker`'s own
    /// root, separate from the dialog's own `focus` so tab order/ARIA state
    /// (`role(ComboBox)`, `aria_expanded`) is scoped to just this control.
    date_focus: FocusHandle,
    /// The selected date lives in here (`gpui_base`'s own controlled
    /// day-grid), not a duplicate field — `selected_date` reads it directly.
    calendar: Entity<CalendarState>,
    date_open: bool,
    /// Absolute bounds of the date-picker trigger, same convention as
    /// `org_bounds`.
    date_bounds: PopoverAnchor,
    split_meeting: bool,
    unified_time: Entity<InputState>,
    quali_time: Entity<InputState>,
    finale_time: Entity<InputState>,
    persons: Vec<Entity<InputState>>,
    _subs: Vec<Subscription>,
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
        let calendar = cx.new(|cx| CalendarState::new(window, cx));
        let unified_time = field("HH:MM", window, cx);
        let quali_time = field("HH:MM", window, cx);
        let finale_time = field("HH:MM", window, cx);

        let subs = vec![
            // The calendar's own navigation (prev/next month, view toggles)
            // only `cx.notify()`s itself — it never becomes an event — so a
            // plain observe is needed to re-render the dialog around it.
            cx.observe(&calendar, |_, _, cx| cx.notify()),
            // Selecting a single day is a *complete* pick — close the popover
            // the same instant it's chosen, rather than requiring a second
            // click outside.
            cx.subscribe(&calendar, |this, _, event: &CalendarEvent, cx| {
                let CalendarEvent::Selected(_) = event;
                this.date_open = false;
                cx.notify();
            }),
            Self::wire_time_input(&unified_time, window, cx),
            Self::wire_time_input(&quali_time, window, cx),
            Self::wire_time_input(&finale_time, window, cx),
        ];

        Self {
            target: None,
            creating: false,
            on_delete,
            on_create,
            open: false,
            focus: cx.focus_handle(),
            name: field(&cx.t("detail.meta.name-placeholder"), window, cx),
            location: field(&cx.t("detail.meta.location-placeholder"), window, cx),
            organization: OrganizationDTO::DTB,
            org_open: false,
            org_bounds: PopoverAnchor::new(),
            date_focus: cx.focus_handle(),
            calendar,
            date_open: false,
            date_bounds: PopoverAnchor::new(),
            split_meeting: false,
            unified_time,
            quali_time,
            finale_time,
            persons: Vec::new(),
            _subs: subs,
        }
    }

    /// Auto-format + finalize one meeting-time field: colons are inserted as
    /// the user types raw digits (see [`auto_format_time`]), and on blur the
    /// value is reformatted to the zero-padded canonical form (see
    /// [`canonical_time`]) — so a single-digit-hour `2:34` the user typed
    /// with their own `:` still ends up `02:34` once they move on.
    fn wire_time_input(
        input: &Entity<InputState>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Subscription {
        cx.subscribe_in(input, window, |_, input, event, window, cx| match event {
            InputEvent::Change => {
                let raw = input.read(cx).value().to_string();
                let formatted = auto_format_time(&raw);
                if formatted != raw {
                    input.update(cx, |input, cx| input.set_value(formatted, window, cx));
                }
            }
            InputEvent::Blur => {
                let raw = input.read(cx).value().to_string();
                if let Some(canonical) = canonical_time(&raw)
                    && canonical != raw
                {
                    input.update(cx, |input, cx| input.set_value(canonical, window, cx));
                }
            }
            _ => {}
        })
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
        self.date_open = false;
        self.calendar
            .update(cx, |cal, cx| cal.set_date(meta.date, window, cx));

        let set = |input: &Entity<InputState>,
                   value: String,
                   window: &mut Window,
                   cx: &mut Context<Self>| {
            input.update(cx, |input, cx| input.set_value(value, window, cx));
        };
        set(&self.name, meta.name.clone(), window, cx);
        set(&self.location, meta.location.clone(), window, cx);

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

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.target = None;
        self.creating = false;
        self.org_open = false;
        self.date_open = false;
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

    /// The date picked in the calendar — `CalendarState` is the sole source
    /// of truth, no separate field to keep in sync.
    fn selected_date(&self, cx: &Context<Self>) -> Option<NaiveDate> {
        self.calendar.read(cx).date().start()
    }

    fn can_save(&self, cx: &Context<Self>) -> bool {
        !self.creating || (!self.parsed_name(cx).is_empty() && self.selected_date(cx).is_some())
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
                gpui_kit::PromptButton::new(cx.t("detail.meta.delete-button")),
                gpui_kit::PromptButton::new(cx.t("detail.meta.delete-cancel-button")),
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
        let date = self.selected_date(cx);
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
        let list = open
            .then(|| self.org_bounds.get())
            .flatten()
            .map(|b| {
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
                col
            })
            // `paint_deferred_draws` paints deferred layers in a flat priority
            // sort, and `gpui_kit::base::Dialog` sits at `10 + layer`; `POPUP_PRIORITY`
            // (100) is gpui-base's convention for "above a dialog". Without this
            // the dialog panel paints over the popover and it's invisible.
            .and_then(|col| self.org_bounds.float_below(px(3.), col));

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
            .child(self.org_bounds.probe())
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

    /// The date field: a `gpui_kit::base::DatePicker`/`Calendar` pair (behaviour +
    /// structure) with our own trigger + popover chrome layered on top —
    /// same canvas-probe + `deferred(anchored())` + `POPUP_PRIORITY`
    /// convention as `org_selector` above, so it floats above the dialog
    /// panel without affecting its layout height.
    fn date_picker(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.date_open;
        let creating = self.creating;
        let selected = self.selected_date(cx);
        let invalid = creating && selected.is_none();
        let label: SharedString = selected
            .map(|d| SharedString::from(d.format(DATE_FMT).to_string()))
            .unwrap_or_else(|| cx.t("detail.meta.date-placeholder"));

        let popover = open
            .then(|| {
                div()
                    .id("date-popover")
                    .occlude()
                    // On the panel itself, not the (much smaller) trigger — the
                    // trigger's hitbox never covers where this panel actually
                    // paints (an anchored overlay). `Calendar`'s day/month/year
                    // cells select on `on_click` (mouse-up), one full event
                    // dispatch *after* the mouse-down that opens/closes a
                    // popover; attaching this to the trigger closed the popover
                    // on the down-half of every click inside it (the panel
                    // un-rendered before the up-half could ever reach the cell),
                    // so a day pick never registered — same fix as
                    // `remarks.rs`'s color panel.
                    .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                        if this.date_open {
                            this.date_open = false;
                            cx.notify();
                        }
                    }))
                    .p(px(10.))
                    .rounded(radius)
                    .border_1()
                    .border_color(c.border)
                    .bg(c.surface)
                    .shadow_lg()
                    .child(self.calendar_element(cx))
            })
            // Same priority rationale as `org_selector`'s own popover —
            // `Dialog` paints at priority 10, so anything meant to float
            // above it needs `POPUP_PRIORITY` (100).
            .and_then(|panel| self.date_bounds.float_below(px(3.), panel));

        let entity = cx.entity().downgrade();

        DatePicker::new("meta-date-picker", &self.date_focus)
            .open(open)
            .on_open_change(move |open, _window, cx| {
                _ = entity.update(cx, |this, cx| {
                    this.date_open = open;
                    cx.notify();
                });
            })
            .relative()
            .flex()
            .flex_col()
            .child(self.date_bounds.probe())
            .child(
                div()
                    .id("date-select-trigger")
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(radius)
                    .border_1()
                    .border_color(if invalid {
                        c.critical
                    } else if open {
                        c.primary
                    } else {
                        c.border
                    })
                    .bg(c.surface)
                    .text_size(px(13.))
                    .cursor_pointer()
                    // Always *opens* (never toggles closed) — the panel's own
                    // `on_mouse_down_out` already fires (capture phase, ahead
                    // of this click's bubble-phase handler) for a click
                    // anywhere outside the panel, including the trigger
                    // itself; a naive toggle here would read that already-
                    // mutated `false` and flip it straight back to `true`.
                    .on_click(cx.listener(|this, _, _w, cx| {
                        if !this.date_open {
                            this.date_open = true;
                            cx.notify();
                        }
                    }))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .when(selected.is_none(), |el| el.text_color(c.muted_foreground))
                            .child(label),
                    )
                    .child(Icon::Calendar.size(px(14.)).color(c.muted_foreground)),
            )
            .children(popover)
    }

    /// Our styling layered on `gpui_base`'s unstyled `Calendar`/`CalendarState`
    /// — base owns the grid math, navigation and selection, this closure just
    /// decorates the pre-wired item slot per [`CalendarItemKind`]/
    /// [`CalendarItemState`] (same division of labour as every other
    /// `gpui-base` primitive in this codebase).
    fn calendar_element(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();

        let weekdays: Vec<SharedString> = (0..7)
            .map(|n| cx.t(format!("detail.meta.calendar-weekday-{n}").as_str()))
            .collect();
        let months: Vec<SharedString> = std::iter::once(SharedString::default())
            .chain((1..=12).map(|n| cx.t(format!("detail.meta.calendar-month-{n}").as_str())))
            .collect();

        Calendar::new("meta-date-calendar", &self.calendar)
            .first_day_of_week(Weekday::Mon)
            .w(px(228.))
            .gap(px(2.))
            .label(move |kind, value| match kind {
                CalendarItemKind::Previous => "‹".into(),
                CalendarItemKind::Next => "›".into(),
                CalendarItemKind::Weekday => weekdays[value as usize].clone(),
                CalendarItemKind::MonthToggle | CalendarItemKind::Month => {
                    months[value as usize].clone()
                }
                _ => value.to_string().into(),
            })
            .item(move |item, state, _window, _cx| {
                match state.kind() {
                    CalendarItemKind::Previous | CalendarItemKind::Next => item
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius)
                        .text_color(c.muted_foreground)
                        .when(state.is_disabled(), |el| el.opacity(0.35))
                        .when(!state.is_disabled(), |el| {
                            el.cursor_pointer().hover(|el| el.bg(c.accent_soft))
                        }),
                    CalendarItemKind::MonthToggle | CalendarItemKind::YearToggle => item
                        .px(px(6.))
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(state.is_active(), |el| {
                            el.bg(c.accent_soft).text_color(c.primary)
                        })
                        .hover(|el| el.bg(c.accent_soft)),
                    CalendarItemKind::Weekday => item
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(10.5))
                        .text_color(c.muted_foreground),
                    CalendarItemKind::Day => item
                        .size(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius)
                        .text_size(px(12.))
                        .when(!state.is_disabled(), |el| el.cursor_pointer())
                        .when(state.is_muted(), |el| {
                            el.text_color(c.muted_foreground).opacity(0.5)
                        })
                        .when(state.is_today() && !state.is_active(), |el| {
                            el.border_1().border_color(c.primary)
                        })
                        .when(state.is_active(), |el| {
                            el.bg(c.primary).text_color(c.primary_foreground)
                        })
                        .when(!state.is_disabled() && !state.is_active(), |el| {
                            el.hover(|el| el.bg(c.accent_soft))
                        }),
                    CalendarItemKind::Month | CalendarItemKind::Year => item
                        // Fill the grid cell rather than a fixed pixel width —
                        // `Calendar`'s own grid (`grid_cols(3)`/`grid_cols(5)`)
                        // divides the container into equal `1fr` tracks; a
                        // fixed width here doesn't match that division, which
                        // left the month grid ragged/off-centre and, for the
                        // 5-column year grid, overflowed the popover outright.
                        .w_full()
                        .h(px(24.))
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(radius)
                        .text_size(px(12.))
                        .cursor_pointer()
                        .when(state.is_active(), |el| {
                            el.bg(c.primary).text_color(c.primary_foreground)
                        })
                        .when(!state.is_active(), |el| el.hover(|el| el.bg(c.accent_soft))),
                }
                .into_any_element()
            })
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
                      label: gpui_kit::SharedString,
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
                        .child(self.date_picker(cx))
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
    label: impl Into<gpui_kit::SharedString>,
    input: &Entity<InputState>,
    c: &crate::theme::PaletteColors,
    radius: gpui_kit::Pixels,
) -> gpui_kit::Div {
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
                .child(gpui_kit::base::input::Input::new(input)),
        )
}

/// Live-typing mask for a meeting-time field: once a third raw digit arrives
/// with no `:` yet, a colon is inserted after the first two (`"023"` →
/// `"02:3"`, finishing `"0234"` → `"02:34"`) — a lone digit is ambiguous
/// (still could become the first of two hour digits) so no colon is inserted
/// for it, matching that the user can instead type an explicit `2:34`
/// themselves for a single-digit hour ([`canonical_time`] zero-pads that on
/// blur). A string that already contains `:` is left untouched — the mask
/// never overrides a separator the user placed themselves.
fn auto_format_time(raw: &str) -> String {
    if raw.contains(':') {
        return raw.to_string();
    }
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).take(4).collect();
    if digits.len() < 3 {
        digits
    } else {
        format!("{}:{}", &digits[..2], &digits[2..])
    }
}

/// The zero-padded canonical form of a time string, if it parses at all
/// (chrono's `%H`/`%M` already tolerate an unpadded single digit on input —
/// `"2:34"` parses as `02:34:00` — this just re-renders that through
/// [`TIME_FMT`] so the field shows it). `None` for a still-incomplete or
/// invalid value, which callers leave untouched rather than clobber.
fn canonical_time(raw: &str) -> Option<String> {
    NaiveTime::parse_from_str(raw.trim(), TIME_FMT)
        .ok()
        .map(|t| t.format(TIME_FMT).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_format_inserts_a_colon_once_a_third_digit_arrives() {
        assert_eq!(auto_format_time("0234"), "02:34");
        assert_eq!(auto_format_time("1745"), "17:45");
        assert_eq!(auto_format_time("023"), "02:3");
    }

    #[test]
    fn auto_format_leaves_one_or_two_bare_digits_alone() {
        assert_eq!(auto_format_time(""), "");
        assert_eq!(auto_format_time("2"), "2");
        assert_eq!(auto_format_time("02"), "02");
    }

    #[test]
    fn auto_format_never_overrides_a_colon_the_user_already_typed() {
        assert_eq!(auto_format_time("2:34"), "2:34");
        assert_eq!(auto_format_time("02:34"), "02:34");
        assert_eq!(auto_format_time("2:"), "2:");
    }

    #[test]
    fn auto_format_caps_at_four_raw_digits() {
        assert_eq!(auto_format_time("023456"), "02:34");
    }

    #[test]
    fn canonical_time_zero_pads_a_single_digit_hour() {
        assert_eq!(canonical_time("2:34"), Some("02:34".to_string()));
        assert_eq!(canonical_time("23:4"), Some("23:04".to_string()));
        assert_eq!(canonical_time("02:34"), Some("02:34".to_string()));
    }

    #[test]
    fn canonical_time_rejects_incomplete_or_invalid_input() {
        assert_eq!(canonical_time(""), None);
        assert_eq!(canonical_time("2:"), None);
        assert_eq!(canonical_time("garbage"), None);
        assert_eq!(canonical_time("25:99"), None);
    }
}
