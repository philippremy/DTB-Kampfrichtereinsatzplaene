//! The detail / editing pane for the selected competition.
//!
//! [`DetailView`] binds to `AppStore`'s current selection. Editable text lives
//! in `gpui_base` `InputState` / `TextareaState` entities that this view and its
//! children own; on change they write straight back to the `CompetitionDocument`
//! editor entities, which drives the debounced autosave.
//!
//! Structural changes (selecting another competition, switching phase, adding /
//! removing a table) rebuild the affected child entities inside an `observe_in`
//! / `update_in` callback, where a `&mut Window` is available.

mod dialog_frame;
mod judging_table;
mod meta_dialog;
mod remarks;
mod spare_judges;
mod table_wizard;

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, App, AppContext, Bounds, Context, DragMoveEvent, Entity, EntityId,
    InteractiveElement, IntoElement, ParentElement, Pixels, PromptLevel, Render, ScrollHandle,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, canvas, div,
    ease_out_quint, px,
};
use gpui_base::Scrollbar;
use uuid::Uuid;

use crate::components::segmented::Segmented;
use crate::components::template_tile::TemplateTile;
use crate::detail::judging_table::{
    CardHandlers, DeleteHandler, DragTable, DuplicateHandler, JudgingTableCard, SelectHandler,
};
use crate::detail::meta_dialog::{CreateRequest, DeleteRequest, MetaDialog};
use crate::detail::remarks::RemarksSection;
use crate::detail::spare_judges::SpareJudgesSection;
use crate::detail::table_wizard::TableWizard;
use crate::i18n::ActiveLocale;
use crate::model::{self, JudgingTable, PhaseConflicts};
use crate::store::{AppStore, CompetitionDocument, DocumentEvent, JudgingTableEditor, RoundEditor};
use crate::theme::{ActiveTheme, PaletteColors};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    #[default]
    Qualification,
    Finale,
}

pub struct DetailView {
    store: Entity<AppStore>,
    doc_id: Option<Uuid>,
    doc: Option<Entity<CompetitionDocument>>,
    phase: Phase,

    cards: Vec<Entity<JudgingTableCard>>,
    selected_table: Option<EntityId>,
    spare: Entity<SpareJudgesSection>,
    remarks: Entity<RemarksSection>,
    wizard: Entity<TableWizard>,
    meta_dialog: Entity<MetaDialog>,

    /// Judge double-bookings in the active phase; recomputed on every edit.
    conflicts: PhaseConflicts,

    /// `true` once the detail toolbar is too narrow to show button labels — set
    /// from a `canvas` width probe (see [`Self::detail_toolbar`]), read on the
    /// next frame. Interior mutability so the probe can update it without a
    /// re-entrant entity borrow.

    /// Scroll position of the content pane, so a re-render keeps it.
    scroll: ScrollHandle,

    /// The `#detail-scroll` pane's own current window-relative visible
    /// bounds, captured by a `canvas` probe in `Render` — shared with every
    /// card (judging table / spare judges / remarks) so their native glass
    /// regions can clip themselves to it on the glass tier (see
    /// `crate::skin::glass::region_in_viewport`). `None` until the pane has
    /// painted once.
    card_viewport: Rc<Cell<Option<Bounds<Pixels>>>>,

    /// While a reordering drag is in progress: the order the cards would
    /// land in if dropped right now (computed live purely from cursor
    /// position — see [`Self::update_drag_preview`]), purely a rendering
    /// concern that never touches `RoundEditor`'s real order until an actual
    /// drop ([`Self::reorder_table`]). Read only while `cx.has_active_drag()`
    /// (see [`Self::ordered_cards`]) since nothing explicitly clears it once
    /// a drag ends.
    drag_preview: Option<Vec<EntityId>>,

    _subs: Vec<Subscription>,
    /// Observer on the active round — recomputes [`Self::conflicts`] on any
    /// judge-name change. Rebuilt whenever the active round changes.
    _round_sub: Vec<Subscription>,
    /// Subscription to the bound document's [`DocumentEvent`]s (undo / redo).
    /// Rebuilt only when the bound document itself changes.
    _doc_sub: Option<Subscription>,
}

impl DetailView {
    pub fn new(store: Entity<AppStore>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let card_viewport: Rc<Cell<Option<Bounds<Pixels>>>> = Rc::new(Cell::new(None));
        let spare = cx.new(|cx| SpareJudgesSection::new(card_viewport.clone(), window, cx));
        let remarks = cx.new(|cx| RemarksSection::new(card_viewport.clone(), window, cx));

        let on_created: table_wizard::Created = {
            let weak = cx.weak_entity();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                weak.update(cx, |view, cx| {
                    view.selected_table = None;
                    view.rebuild_phase(window, cx);
                })
                .ok();
            })
        };
        let wizard = cx.new(|cx| TableWizard::new(on_created, window, cx));

        let on_delete_competition: DeleteRequest = {
            let weak = cx.weak_entity();
            Rc::new(move |_window: &mut Window, cx: &mut App| {
                weak.update(cx, |view, cx| {
                    if let Some(id) = view.doc_id {
                        view.store
                            .update(cx, |store, cx| store.delete_competition(id, cx));
                    }
                })
                .ok();
            })
        };
        let on_create_competition: CreateRequest = {
            let weak = cx.weak_entity();
            Rc::new(move |meta, _window: &mut Window, cx: &mut App| {
                weak.update(cx, |view, cx| {
                    view.store
                        .update(cx, |store, cx| store.create_with_meta(meta, cx));
                })
                .ok();
            })
        };
        let meta_dialog =
            cx.new(|cx| MetaDialog::new(on_delete_competition, on_create_competition, window, cx));

        let subs = vec![cx.observe_in(&store, window, |this, _, window, cx| {
            if this.store.read(cx).selected_id() != this.doc_id {
                this.rebind(window, cx);
            }
        })];

        let mut this = Self {
            store,
            doc_id: None,
            doc: None,
            phase: Phase::default(),
            cards: Vec::new(),
            selected_table: None,
            spare,
            remarks,
            wizard,
            meta_dialog,
            conflicts: PhaseConflicts::default(),
            scroll: ScrollHandle::new(),
            card_viewport,
            drag_preview: None,
            _subs: subs,
            _round_sub: Vec::new(),
            _doc_sub: None,
        };
        this.rebind(window, cx);
        this
    }

    fn active_round(&self, cx: &App) -> Option<Entity<RoundEditor>> {
        let doc = self.doc.as_ref()?.read(cx);
        Some(match self.phase {
            Phase::Qualification => doc.qualification().clone(),
            Phase::Finale => doc.finale().clone(),
        })
    }

    fn selected_table_editor(&self, cx: &App) -> Option<Entity<JudgingTableEditor>> {
        let id = self.selected_table?;
        self.table_editor_by_id(id, cx)
    }

    fn table_editor_by_id(&self, id: EntityId, cx: &App) -> Option<Entity<JudgingTableEditor>> {
        self.cards
            .iter()
            .find(|card| card.read(cx).editor_id() == id)
            .map(|card| card.read(cx).table_editor())
    }

    fn set_phase(&mut self, phase: Phase, window: &mut Window, cx: &mut Context<Self>) {
        if self.phase != phase {
            self.phase = phase;
            self.selected_table = None;
            self.rebuild_phase(window, cx);
            cx.notify();
        }
    }

    fn select_table(&mut self, id: EntityId, cx: &mut Context<Self>) {
        self.selected_table = Some(id);
        for card in &self.cards {
            let is_selected = card.read(cx).editor_id() == id;
            card.update(cx, |card, cx| card.set_selected(is_selected, cx));
        }
        cx.notify();
    }

    /// Whether a judging table is currently selected (drives the detail toolbar
    /// and the Edit menu).
    pub(crate) fn has_table_selection(&self) -> bool {
        self.selected_table.is_some()
    }

    pub(crate) fn open_wizard(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(round) = self.active_round(cx) else {
            return;
        };
        self.wizard
            .update(cx, |wizard, cx| wizard.open_for(round, window, cx));
    }

    pub(crate) fn open_meta_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(doc) = self.doc.clone() else {
            return;
        };
        let meta = doc.read(cx).metadata().clone();
        self.meta_dialog
            .update(cx, |dialog, cx| dialog.open_for(meta, window, cx));
    }

    /// Open the metadata dialog in "new competition" mode — nothing is added to
    /// the database until the user fills it in and saves.
    pub(crate) fn open_new_competition_dialog(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.meta_dialog
            .update(cx, |dialog, cx| dialog.open_for_new(window, cx));
    }

    pub(crate) fn request_duplicate(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_table {
            self.duplicate_table(id, window, cx);
        }
    }

    /// Duplicate one judging table (by its editor id). Used by both the
    /// toolbar / Edit menu (via [`Self::request_duplicate`], the *selected*
    /// table) and a card's own context menu (an explicit id, which need not
    /// be selected).
    pub(crate) fn duplicate_table(
        &mut self,
        editor_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (Some(editor), Some(round)) = (
            self.table_editor_by_id(editor_id, cx),
            self.active_round(cx),
        ) else {
            return;
        };
        let table = editor.read(cx).table().clone();
        let copy_label = cx.t_fmt("detail.duplicate-table-label", &[("label", &table.label)]);
        round.update(cx, |round, cx| {
            round.add_table(
                JudgingTable {
                    label: copy_label,
                    kind: table.kind,
                },
                cx,
            );
        });
        self.selected_table = None;
        self.rebuild_phase(window, cx);
        cx.notify();
    }

    pub(crate) fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(id) = self.selected_table {
            self.delete_table(id, window, cx);
        }
    }

    /// Confirm, then remove one judging table (by its editor id). Used by both
    /// the per-card trash button and the detail toolbar / Edit menu.
    pub(crate) fn delete_table(
        &mut self,
        editor_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(round) = self.active_round(cx) else {
            return;
        };
        let label = round
            .read(cx)
            .tables()
            .iter()
            .find(|t| t.entity_id() == editor_id)
            .map(|t| t.read(cx).table().label.trim().to_owned())
            .unwrap_or_default();
        let detail = if label.is_empty() {
            cx.t("detail.delete-table-detail-unnamed").to_string()
        } else {
            cx.t_fmt("detail.delete-table-detail", &[("label", &label)])
        };

        let answer = window.prompt(
            PromptLevel::Warning,
            &cx.t("detail.delete-table-confirm"),
            Some(&detail),
            &[
                gpui::PromptButton::new(cx.t("detail.delete-table-delete-button")),
                gpui::PromptButton::new(cx.t("detail.delete-table-cancel-button")),
            ],
            cx,
        );
        cx.spawn_in(window, async move |view, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            cx.update(|window, cx| {
                view.update(cx, |view, cx| view.remove_table_now(editor_id, window, cx))
                    .ok();
            })
            .ok();
        })
        .detach();
    }

    /// Drop of `dragged` onto `target` — move it to the target's position.
    /// Commit a reordering drag on drop, straight from the live preview's
    /// own computed landing index (`self.drag_preview`) — not by resolving
    /// whatever specific card the drop landed on: the drop point is very
    /// often the dragged card's *own* (now-relocated, empty-looking)
    /// placeholder slot, the whole point of the live preview, which a
    /// target-card-based resolution can't express (`target == dragged`
    /// there is indistinguishable from a genuine no-op drop). No preview
    /// (e.g. a drop with no prior move event — possible for a very fast
    /// click-drag-release right at the drag threshold) is just a no-op.
    fn reorder_table(&mut self, dragged: EntityId, window: &mut Window, cx: &mut Context<Self>) {
        let Some(round) = self.active_round(cx) else {
            return;
        };
        let Some(preview) = self.drag_preview.take() else {
            return;
        };
        let real: Vec<EntityId> = round
            .read(cx)
            .tables()
            .iter()
            .map(|t| t.entity_id())
            .collect();
        let (Some(from), Some(to)) = (
            real.iter().position(|id| *id == dragged),
            preview.iter().position(|id| *id == dragged),
        ) else {
            return;
        };
        if from == to {
            return;
        }
        round.update(cx, |round, cx| round.move_table(from, to, cx));
        self.rebuild_phase(window, cx);
        cx.notify();
    }

    /// Recompute the live reorder preview from the cards container's own
    /// `on_drag_move` (see `phase_body`) — purely from `cursor_y` (relative
    /// to the first card's top) and each card's own [`JudgingTableCard::
    /// natural_height`], never from "which card is currently under the
    /// pointer".
    ///
    /// An earlier version had each card report its own hover state instead,
    /// which was unstable by construction: once the preview reorders the
    /// list, a *different* card slides under the still-stationary pointer,
    /// which reports itself as hovered, which reorders the list again,
    /// which slides yet another card under the pointer — a feedback loop
    /// that visibly thrashed before settling. Deriving the target index from
    /// the cursor's raw position and each card's *content-driven* (so
    /// order-independent) height instead breaks that loop: the computation
    /// no longer depends on the list's current (already-reordered) layout at
    /// all.
    fn update_drag_preview(&mut self, dragged: EntityId, cursor_y: Pixels, cx: &mut Context<Self>) {
        let gap = px(10.);
        let mut cumulative = px(0.);
        let mut order = Vec::with_capacity(self.cards.len());
        let mut insert_at = None;
        for card in &self.cards {
            let card = card.read(cx);
            let id = card.editor_id();
            if id == dragged {
                continue;
            }
            let height = card.natural_height().unwrap_or(px(0.));
            if insert_at.is_none() && cursor_y < cumulative + height * 0.5 {
                insert_at = Some(order.len());
            }
            order.push(id);
            cumulative += height + gap;
        }
        let insert_at = insert_at.unwrap_or(order.len());
        order.insert(insert_at, dragged);

        if self.drag_preview.as_ref() != Some(&order) {
            self.drag_preview = Some(order);
            cx.notify();
        }
    }

    /// The cards in their live reorder-preview order while a drag is
    /// actually in progress, else their real (`self.cards`) order.
    /// `cx.has_active_drag()` is the authoritative gate — `drag_preview`
    /// itself is never explicitly cleared when a drag ends (there's no such
    /// hook; see `JudgingTableCard`'s identical `dragging` gate) — so using
    /// it un-gated could replay a stale preview for one frame at the start
    /// of an unrelated later drag.
    fn ordered_cards(&self, cx: &Context<Self>) -> Vec<Entity<JudgingTableCard>> {
        if cx.has_active_drag()
            && let Some(order) = &self.drag_preview
        {
            return order
                .iter()
                .filter_map(|id| {
                    self.cards
                        .iter()
                        .find(|card| card.read(cx).editor_id() == *id)
                        .cloned()
                })
                .collect();
        }
        self.cards.clone()
    }

    fn remove_table_now(
        &mut self,
        editor_id: EntityId,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(round) = self.active_round(cx) else {
            return;
        };
        if let Some(index) = round
            .read(cx)
            .tables()
            .iter()
            .position(|t| t.entity_id() == editor_id)
        {
            round.update(cx, |round, cx| round.remove_table(index, cx));
            if self.selected_table == Some(editor_id) {
                self.selected_table = None;
            }
            self.rebuild_phase(window, cx);
            cx.notify();
        }
    }

    fn rebind(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let new_doc = self.store.read(cx).selected_document().cloned();
        let doc_changed =
            self.doc.as_ref().map(|d| d.entity_id()) != new_doc.as_ref().map(|d| d.entity_id());

        self.doc_id = self.store.read(cx).selected_id();
        self.doc = new_doc;
        self.selected_table = None;

        // (Re)subscribe to the document for undo / redo restores — only when the
        // bound document actually changed, so a restore-triggered `rebind` does
        // not tear down the subscription it is running inside.
        if doc_changed {
            self._doc_sub = self.doc.as_ref().map(|doc| {
                cx.subscribe_in(doc, window, |this, _, event: &DocumentEvent, window, cx| {
                    match event {
                        DocumentEvent::Restored => this.rebind(window, cx),
                    }
                })
            });
        }

        if let Some(doc) = self.doc.clone() {
            let remarks_editor = doc.read(cx).remarks().clone();
            self.remarks
                .update(cx, |section, cx| section.rebind(remarks_editor, window, cx));
        } else {
            self.remarks
                .update(cx, |section, cx| section.clear(window, cx));
        }

        self.rebuild_phase(window, cx);
        cx.notify();
    }

    fn rebuild_phase(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.cards.clear();
        self.drag_preview = None;

        let Some(round) = self.active_round(cx) else {
            self.spare
                .update(cx, |section, cx| section.clear(window, cx));
            self.conflicts = PhaseConflicts::default();
            self._round_sub.clear();
            return;
        };

        let card_viewport = self.card_viewport.clone();
        for editor in round.read(cx).tables().to_vec() {
            let id = editor.entity_id();
            let on_select: SelectHandler = {
                let weak = cx.weak_entity();
                Rc::new(move |cx: &mut App| {
                    weak.update(cx, |view, cx| view.select_table(id, cx)).ok();
                })
            };
            let on_delete: DeleteHandler = {
                let weak = cx.weak_entity();
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    weak.update(cx, |view, cx| view.delete_table(id, window, cx))
                        .ok();
                })
            };
            let on_duplicate: DuplicateHandler = {
                let weak = cx.weak_entity();
                Rc::new(move |window: &mut Window, cx: &mut App| {
                    weak.update(cx, |view, cx| view.duplicate_table(id, window, cx))
                        .ok();
                })
            };
            let card = cx.new(|cx| {
                JudgingTableCard::new(
                    editor,
                    CardHandlers {
                        on_select,
                        on_delete,
                        on_duplicate,
                    },
                    card_viewport.clone(),
                    window,
                    cx,
                )
            });
            self.cards.push(card);
        }

        let spare_values = round.read(cx).spare_judges().to_vec();
        self.spare.update(cx, |section, cx| {
            section.rebind(round.clone(), spare_values, window, cx)
        });

        // A judge-name edit in any table / the spare list re-flows through the
        // `RoundEditor`, so one observer on it keeps the conflict view live.
        self._round_sub = vec![cx.observe(&round, |this, _, cx| this.refresh_conflicts(cx))];
        self.refresh_conflicts(cx);
    }

    /// Recompute the active phase's conflicts and push the per-card / per-field
    /// highlights.
    fn refresh_conflicts(&mut self, cx: &mut Context<Self>) {
        let Some(round) = self.active_round(cx) else {
            self.conflicts = PhaseConflicts::default();
            cx.notify();
            return;
        };
        let (tables, spare): (Vec<JudgingTable>, Vec<String>) = {
            let round = round.read(cx);
            (
                round
                    .tables()
                    .iter()
                    .map(|editor| editor.read(cx).table().clone())
                    .collect(),
                round.spare_judges().to_vec(),
            )
        };

        let conflicts = model::detect_phase(&tables, &spare);

        for (index, card) in self.cards.iter().enumerate() {
            let roles = conflicts.table_roles(index);
            card.update(cx, |card, cx| card.set_conflict_roles(roles, cx));
        }
        let spare_names = conflicts.spare_names();
        self.spare.update(cx, |section, cx| {
            section.set_conflict_names(spare_names, cx)
        });

        self.conflicts = conflicts;
        cx.notify();
    }

    fn conflict_banner(&self, cx: &Context<Self>) -> Option<impl IntoElement> {
        if self.conflicts.is_empty() {
            return None;
        }
        let c = cx.theme().color;
        Some(
            div()
                .mx(px(20.))
                .mt(px(12.))
                .flex()
                .flex_col()
                .gap(px(3.))
                .p(px(10.))
                .rounded(cx.theme().skin.radius_px())
                .border_1()
                .border_color(c.warn)
                .bg(gpui::Hsla { a: 0.10, ..c.warn })
                .text_size(px(11.5))
                .child(
                    div()
                        .font_weight(gpui::FontWeight::SEMIBOLD)
                        .child(cx.t_plural("detail.conflicts", self.conflicts.len() as i64, &[])),
                )
                .children(self.conflicts.iter().map(|conflict| {
                    div()
                        .text_color(c.muted_foreground)
                        .child(conflict.summary())
                })),
        )
    }

    /// The phase-scoped content (conflict banner + judging-table cards + spare
    /// judges), wrapped in a short cross-fade that replays on every
    /// Qualifikation ⇄ Finale switch. Remarks are document-wide and stay put.
    fn phase_body(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let weak = cx.weak_entity();
        let motion = cx.theme().skin.motion(Duration::from_millis(150));
        let phase_key = match self.phase {
            Phase::Qualification => 0usize,
            Phase::Finale => 1,
        };

        div()
            .flex()
            .flex_col()
            .children(self.conflict_banner(cx))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .p(px(20.))
                    // The live reorder preview + its actual commit on drop are
                    // both handled *here*, centrally, rather than per-card —
                    // see `update_drag_preview`'s and `reorder_table`'s doc
                    // comments for why a per-card version was unstable and
                    // couldn't express dropping onto the dragged card's own
                    // (relocated) placeholder slot.
                    .on_drag_move::<DragTable>(cx.listener(
                        |this, event: &DragMoveEvent<DragTable>, _window, cx| {
                            let dragged = event.drag(cx).editor_id();
                            // `.p(px(20.))` above — the first card starts 20px
                            // below this container's own (padding-included)
                            // top edge, which is what `event.bounds` reports.
                            let cursor_y = event.event.position.y - event.bounds.origin.y - px(20.);
                            this.update_drag_preview(dragged, cursor_y, cx);
                        },
                    ))
                    .on_drop(cx.listener(|this, dragged: &DragTable, window, cx| {
                        this.reorder_table(dragged.editor_id(), window, cx);
                    }))
                    .children(self.ordered_cards(cx))
                    .child(
                        TemplateTile::card("add-table-template")
                            .label(cx.t("detail.add-table-template"))
                            .on_click({
                                let weak = weak.clone();
                                move |_, window, cx| {
                                    weak.update(cx, |view, cx| view.open_wizard(window, cx))
                                        .ok();
                                }
                            }),
                    ),
            )
            .child(self.spare.clone())
            .with_animation(
                ("phase-body", phase_key),
                Animation::new(motion).with_easing(ease_out_quint()),
                |el, t| el.opacity(t),
            )
    }
}

impl Render for DetailView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;

        if self.doc.is_none() {
            return div()
                .flex_1()
                .relative()
                .flex()
                .items_center()
                .justify_center()
                .text_color(c.muted_foreground)
                .child(cx.t("detail.no-selection"))
                // Still mount the dialog so "Neuer Wettkampf" works with no
                // competition selected.
                .child(self.meta_dialog.clone())
                .into_any_element();
        }

        let weak = cx.weak_entity();
        let selected = match self.phase {
            Phase::Qualification => 0,
            Phase::Finale => 1,
        };

        div()
            .flex_1()
            .min_w(px(0.))
            .flex()
            .flex_col()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(
                        div()
                            .id("detail-scroll")
                            .size_full()
                            .flex()
                            .flex_col()
                            // Bottom gutter below the remarks card — its own
                            // `.mb(..)` isn't reliable for the *last* child
                            // of a scroll pane (see `remarks.rs`); this is,
                            // since it's unambiguously part of the
                            // scrollable content's own box.
                            .pb(px(20.))
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(
                                div().flex().px(px(20.)).pt(px(14.)).child(
                                    Segmented::new(
                                        "detail-phase",
                                        [
                                            cx.t("detail.phase-qualification"),
                                            cx.t("detail.phase-finale"),
                                        ],
                                        selected,
                                    )
                                    .on_select({
                                        let weak = weak.clone();
                                        move |index, window, cx| {
                                            let phase = if index == 0 {
                                                Phase::Qualification
                                            } else {
                                                Phase::Finale
                                            };
                                            weak.update(cx, |view, cx| {
                                                view.set_phase(phase, window, cx)
                                            })
                                            .ok();
                                        }
                                    }),
                                ),
                            )
                            .child(self.phase_body(cx))
                            .child(self.remarks.clone()),
                    )
                    // Scroll-edge effect: content fades into the toolbar band
                    // above instead of being cut off by it. It sits over the
                    // scroll area's top edge and doesn't take input.
                    .child(scroll_edge_fade(cx.theme().color.background))
                    .child(Scrollbar::vertical(&self.scroll))
                    .child({
                        // A sibling of `#detail-scroll`, not a descendant —
                        // `#detail-scroll` fills this wrapper exactly
                        // (`size_full`), so their bounds are identical, but
                        // sitting outside the scrolled subtree means this
                        // probe always reports the pane's fixed viewport
                        // box, never a position shifted by the current
                        // scroll offset (unlike a card's own bounds probe,
                        // which is *supposed* to move with scroll). Every
                        // card clips its native glass region to this rect —
                        // see `card_viewport` / `GlassRole::Card`.
                        //
                        // `.top_0().left_0()` are load-bearing, not
                        // decorative: with no inset pinned at all, gpui
                        // falls back to this element's ordinary *flow*
                        // position, and it isn't the first child here (it
                        // comes after `#detail-scroll` itself, the fade, and
                        // the scrollbar) — unpinned, it reported the pane's
                        // *bottom* edge as its bounds instead of the whole
                        // pane.
                        let capture = self.card_viewport.clone();
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
                        .top_0()
                        .left_0()
                        .size_full()
                    }),
            )
            .child(self.wizard.clone())
            .child(self.meta_dialog.clone())
            .into_any_element()
    }
}

pub(super) fn field_label(text: impl Into<SharedString>, c: &PaletteColors) -> gpui::Div {
    let text = text.into();
    div()
        .text_size(px(9.5))
        .text_color(c.muted_foreground)
        .child(text.to_uppercase())
}

/// A short gradient from `fill` (opaque) to transparent, pinned to the top of a
/// `relative` scroll container — the scroll-edge effect for content scrolling
/// under the toolbar band. `fill` must match the band's backing colour.
fn scroll_edge_fade(fill: gpui::Hsla) -> impl IntoElement {
    div()
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .h(px(18.))
        .bg(gpui::linear_gradient(
            180.,
            gpui::linear_color_stop(fill, 0.),
            gpui::linear_color_stop(gpui::Hsla { a: 0.0, ..fill }, 1.),
        ))
}
