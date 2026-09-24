//! The competition-context content of the unified toolbar.
//!
//! An entity so it re-renders only when the selection, the selected
//! competition's metadata, or its save state changes — not on every keystroke
//! elsewhere. The window frame + sidebar toggle around it belong to
//! [`crate::app::AppShell`].

use dtb_ke_types::MeetingTimeDTO;
use gpui_kit::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, prelude::FluentBuilder, px,
};
use uuid::Uuid;

use crate::actions::edit::{DeleteJudgingTable, DuplicateJudgingTable};
use crate::actions::file::{AddJudgingTable, CompetitionSettings, ExportCompetition, NewCompetition};
use crate::actions::window::TogglePreview;
use crate::components::button::{Button, ButtonTone};
use crate::components::icon::Icon;
use crate::components::org_emblem::OrgEmblem;
use crate::components::segmented::Segmented;
use crate::components::toolbar_group::ToolbarGroup;
use crate::detail::DetailView;
use crate::i18n::{ActiveLocale, Locale};
use crate::model::CompetitionMeta;
use crate::preview::PreviewPane;
use crate::store::AppStore;
use crate::theme::ActiveTheme;

pub struct CompetitionToolbar {
    store: Entity<AppStore>,
    /// Read for the table-selection state that enables Duplicate / Delete.
    detail: Entity<DetailView>,
    /// The competition currently being watched via `watch_subs`.
    watched: Option<Uuid>,
    watch_subs: Vec<Subscription>,
    _store_sub: Subscription,
    _detail_sub: Subscription,
    /// The preview pane replacing the editor (iPadOS) toggles the button's state.
    _preview_sub: Subscription,
}

impl CompetitionToolbar {
    pub fn new(store: Entity<AppStore>, detail: Entity<DetailView>, cx: &mut Context<Self>) -> Self {
        let store_sub = cx.observe(&store, |this, _, cx| this.rewatch(cx));
        let detail_sub = cx.observe(&detail, |_, _, cx| cx.notify());
        let preview_sub = cx.observe_global::<PreviewPane>(|_, cx| cx.notify());
        let mut this = Self {
            store,
            detail,
            watched: None,
            watch_subs: Vec::new(),
            _store_sub: store_sub,
            _detail_sub: detail_sub,
            _preview_sub: preview_sub,
        };
        this.rewatch(cx);
        this
    }

    /// Keep `watch_subs` pointed at the selected document + its metadata editor
    /// so metadata / save-state edits refresh the toolbar.
    fn rewatch(&mut self, cx: &mut Context<Self>) {
        let selected = self.store.read(cx).selected_id();
        if selected != self.watched {
            self.watched = selected;
            self.watch_subs.clear();
            if let Some(doc) = self.store.read(cx).selected_document().cloned() {
                let metadata = doc.read(cx).metadata().clone();
                self.watch_subs
                    .push(cx.observe(&doc, |_, _, cx| cx.notify()));
                self.watch_subs
                    .push(cx.observe(&metadata, |_, _, cx| cx.notify()));
            }
        }
        cx.notify();
    }
}

impl Render for CompetitionToolbar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;

        let row = div().flex().flex_1().items_center().gap(px(10.)).min_w_0().ml_1();

        let Some(doc) = self.store.read(cx).selected_document().cloned() else {
            return row
                .child(
                    div()
                        .flex_1()
                        .text_color(c.foreground)
                        .text_sm()
                        .font_weight(FontWeight::SEMIBOLD)
                        .child(cx.t("toolbar.no-selection")),
                )
                .child(
                    // Same prominent glass capsule as the Export action.
                    ToolbarGroup::new("toolbar-new").prominent().button(
                        Button::new("new-competition", cx.t("toolbar.new-competition-button"))
                            .tone(ButtonTone::Ghost)
                            .foreground(c.primary_foreground)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewCompetition), cx);
                            }),
                    ),
                );
        };

        let doc = doc.read(cx);
        let meta = doc.metadata().read(cx).meta().clone();
        let locale = cx.global::<Locale>().clone();

        let has_table = self.detail.read(cx).has_table_selection();
        // The preview pane hides the editor, so the table buttons have nothing to act on.
        let previewing = cx.try_global::<PreviewPane>().is_some_and(|pane| pane.showing);
        let pane_mode = crate::skin::window::secondary_windows_as_sheets();
        let label = |key: &str| cx.t(&format!("detail.toolbar-{key}"));

        row.child(OrgEmblem::new(meta.organization))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(-3.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(meta.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.muted_foreground)
                            .truncate()
                            .child(format!(
                                "{} · {}",
                                secondary_line(&meta),
                                meeting_label(&meta.meeting_times, &locale)
                            )),
                    ),
            )
            .child(
                ToolbarGroup::new("toolbar-tables")
                    .button(action(
                        "toolbar-add-table",
                        Icon::Plus,
                        label("add-table"),
                        previewing,
                        || Box::new(AddJudgingTable),
                    ))
                    .button(action(
                        "toolbar-duplicate",
                        Icon::Copy,
                        label("duplicate"),
                        !has_table || previewing,
                        || Box::new(DuplicateJudgingTable),
                    ))
                    .button(action(
                        "toolbar-delete",
                        Icon::Trash,
                        label("delete"),
                        !has_table || previewing,
                        || Box::new(DeleteJudgingTable),
                    )),
            )
            .child({
                let group = ToolbarGroup::new("toolbar-competition").button(action(
                    "toolbar-settings",
                    Icon::Settings,
                    label("settings"),
                    false,
                    || Box::new(CompetitionSettings),
                ));
                // Where the preview replaces the editor in place (iPadOS), it is a two-state pill
                // switch showing both modes; elsewhere it opens a window, so a plain button.
                if pane_mode {
                    group
                } else {
                    group.button(action(
                        "toolbar-preview",
                        Icon::Preview,
                        label("preview"),
                        false,
                        || Box::new(TogglePreview),
                    ))
                }
            })
            .when(pane_mode, |row| {
                row.child(
                    Segmented::new(
                        "toolbar-mode",
                        [label("edit"), label("preview")],
                        usize::from(previewing),
                    )
                    .on_select(move |index, window, cx| {
                        if (index == 1) != previewing {
                            window.dispatch_action(Box::new(TogglePreview), cx);
                        }
                    }),
                )
            })
            .child(
                ToolbarGroup::new("toolbar-export").prominent().button(
                    action(
                        "toolbar-export",
                        Icon::Export,
                        label("export"),
                        false,
                        || Box::new(ExportCompetition),
                    )
                    .foreground(c.primary_foreground),
                ),
            )
    }
}

/// One icon-only toolbar button: dispatches `action` (the same command the
/// menu bar runs), with a tooltip as its accessible name.
fn action(
    id: &'static str,
    icon: Icon,
    label: gpui_kit::SharedString,
    disabled: bool,
    action: impl Fn() -> Box<dyn gpui_kit::Action> + 'static,
) -> Button {
    Button::icon(id, icon)
        .oval()
        .tooltip(label)
        .disabled(disabled)
        .on_click(move |_, window, cx| window.dispatch_action(action(), cx))
}

fn secondary_line(meta: &CompetitionMeta) -> String {
    let date = meta.date.format("%d.%m.%Y");
    let mut parts = Vec::new();
    if !meta.location.trim().is_empty() {
        parts.push(meta.location.clone());
    }
    parts.push(date.to_string());
    parts.push(meta.organization.to_string());
    parts.join(" · ")
}

fn meeting_label(times: &MeetingTimeDTO, locale: &Locale) -> String {
    match times {
        MeetingTimeDTO::Unified(t) => locale.t_fmt(
            "toolbar.meeting-unified",
            &[("time", &t.format("%H:%M").to_string())],
        ),
        MeetingTimeDTO::Split {
            qualification,
            finale,
        } => locale.t_fmt(
            "toolbar.meeting-split",
            &[
                ("qualification", &qualification.format("%H:%M").to_string()),
                ("finale", &finale.format("%H:%M").to_string()),
            ],
        ),
    }
}
