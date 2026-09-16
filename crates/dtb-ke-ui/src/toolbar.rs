//! The competition-context content of the unified toolbar.
//!
//! An entity so it re-renders only when the selection, the selected
//! competition's metadata, or its save state changes — not on every keystroke
//! elsewhere. The window frame + sidebar toggle around it belong to
//! [`crate::app::AppShell`].

use dtb_ke_types::MeetingTimeDTO;
use gpui::{
    Context, Entity, FontWeight, IntoElement, ParentElement, Render, Styled, Subscription, Window,
    div, px,
};
use uuid::Uuid;

use crate::actions::file::NewCompetition;
use crate::components::button::{Button, ButtonTone};
use crate::components::chip::{Chip, ChipTone};
use crate::components::org_emblem::OrgEmblem;
use crate::i18n::{ActiveLocale, Locale};
use crate::model::{self, CompetitionMeta, JudgingTable};
use crate::store::{AppStore, CompetitionDocument, SaveState};
use crate::theme::ActiveTheme;

pub struct CompetitionToolbar {
    store: Entity<AppStore>,
    /// The competition currently being watched via `watch_subs`.
    watched: Option<Uuid>,
    watch_subs: Vec<Subscription>,
    _store_sub: Subscription,
}

impl CompetitionToolbar {
    pub fn new(store: Entity<AppStore>, cx: &mut Context<Self>) -> Self {
        let store_sub = cx.observe(&store, |this, _, cx| this.rewatch(cx));
        let mut this = Self {
            store,
            watched: None,
            watch_subs: Vec::new(),
            _store_sub: store_sub,
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

        let row = div().flex().flex_1().items_center().gap(px(10.)).min_w_0();

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
                    Button::new("new-competition", cx.t("toolbar.new-competition-button"))
                        .tone(ButtonTone::Primary)
                        .on_click(|_, window, cx| {
                            window.dispatch_action(Box::new(NewCompetition), cx);
                        }),
                );
        };

        let doc = doc.read(cx);
        let meta = doc.metadata().read(cx).meta().clone();
        let save = doc.save_state();
        let conflicts = conflict_count(doc, cx);
        let locale = cx.global::<Locale>().clone();

        let (save_label, save_tone) = match &save {
            SaveState::Saved => (locale.t("toolbar.save-state-saved"), ChipTone::Ok),
            SaveState::Dirty => (locale.t("toolbar.save-state-dirty"), ChipTone::Neutral),
            SaveState::Saving => (locale.t("toolbar.save-state-saving"), ChipTone::Neutral),
            SaveState::Error(_) => (locale.t("toolbar.save-state-error"), ChipTone::Critical),
        };

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
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .truncate()
                            .child(meta.name.clone()),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.muted_foreground)
                            .truncate()
                            .child(secondary_line(&meta)),
                    ),
            )
            .child(Chip::new(meeting_label(&meta.meeting_times, &locale)).mono())
            .children((conflicts > 0).then(|| {
                Chip::new(locale.t_plural("toolbar.conflicts", conflicts as i64, &[]))
                    .tone(ChipTone::Critical)
            }))
            .child(Chip::new(save_label).tone(save_tone))
    }
}

/// Total judge double-bookings across both phases (drives the toolbar badge).
fn conflict_count(doc: &CompetitionDocument, cx: &gpui::App) -> usize {
    [doc.qualification(), doc.finale()]
        .into_iter()
        .map(|round| {
            let round = round.read(cx);
            let tables: Vec<JudgingTable> = round
                .tables()
                .iter()
                .map(|editor| editor.read(cx).table().clone())
                .collect();
            model::detect_phase(&tables, round.spare_judges()).len()
        })
        .sum()
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
