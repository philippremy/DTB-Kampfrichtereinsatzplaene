//! The navigation sidebar: competitions grouped by year, with the search field
//! and the "add" button along the bottom.
//!
//! Selection is Finder-style: a click selects one competition (and shows it in
//! the detail pane), Cmd/Ctrl-click adds or removes one, Shift-click selects
//! the range from the last plain/Cmd-clicked row. The most recently clicked
//! competition is the one the detail pane shows; "Löschen" (context menu and
//! File menu) acts on the whole selection.

use std::collections::HashSet;
use std::rc::Rc;

use gpui_kit::FontWeight;
use gpui_kit::{
    App, AppContext, ClickEvent, Context, Entity, Focusable as _, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, PromptLevel, Render, ScrollWheelEvent,
    Size,
    StatefulInteractiveElement, Styled, Subscription, TouchPhase, Window, div,
    prelude::FluentBuilder, px,
    size,
};
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::base::{Scrollbar, VirtualListScrollHandle, v_virtual_list};
use uuid::Uuid;

use crate::actions::file::NewCompetition;
use crate::actions::window::ToggleSidebar;
use crate::components::button::Button;
use crate::skin::glass::{self, GlassRole};
use crate::skin::titlebar;
use crate::components::context_menu::{ContextMenuHandler, ContextMenuItem, context_menu};
use crate::components::field::Field;
use crate::components::focus::sidebar_selection_fill;
use crate::components::icon::Icon;
use crate::components::toolbar_group::ToolbarGroup;
use crate::i18n::ActiveLocale;
use crate::material;
use crate::store::AppStore;
use crate::theme::ActiveTheme;

/// A competition-row action supplied by `AppShell` — reuses its existing
/// handlers (export dialog, competition-settings dialog, preview window)
/// rather than duplicating that wiring here. `id` may not be the current
/// selection yet; implementations select it first (see
/// `AppStore::select_then`).
pub type CompetitionAction = Rc<dyn Fn(Uuid, &mut Window, &mut App)>;

/// Default sidebar width, and the width restored when it is re-opened.
pub const SIDEBAR_WIDTH: Pixels = px(256.);
/// Drag floor — the handle stops here; releasing below [`SIDEBAR_SNAP`] hides it.
pub const SIDEBAR_MIN: Pixels = px(120.);
/// Drag ceiling.
pub const SIDEBAR_MAX: Pixels = px(460.);
/// Releasing the resize handle below this width snaps the sidebar to hidden.
pub const SIDEBAR_SNAP: Pixels = px(172.);

/// Row heights (px) for the virtualised list — the list needs each up front.
const SB_YEAR_H_FIRST: f32 = 18.0;
const SB_YEAR_H: f32 = 30.0;
const SB_COMP_H: f32 = 42.0;
/// Width of the revealed swipe actions (Duplizieren + Löschen).
const SWIPE_ACTIONS_W: f32 = 148.0;

pub struct Sidebar {
    store: Entity<AppStore>,
    search: Entity<InputState>,
    /// Every selected competition. Always contains the store's selected
    /// competition (see [`Self::reconcile`]).
    selection: HashSet<Uuid>,
    /// Where a Shift-click range starts: the last plain- or Cmd-clicked row.
    anchor: Option<Uuid>,
    list_scroll: VirtualListScrollHandle,
    /// The row context menu currently open, if any — the competition id and
    /// the window-absolute point (the right-click) to anchor the popover at.
    context_menu: Option<(Uuid, Point<Pixels>)>,
    /// The row swiped sideways (touch or trackpad) and how far it is pulled open, in px.
    swipe: Option<(Uuid, f32)>,
    /// The selected document whose save state the row indicator shows.
    watched_doc: Option<gpui_kit::EntityId>,
    doc_sub: Option<Subscription>,
    on_export: CompetitionAction,
    on_settings: CompetitionAction,
    on_preview: CompetitionAction,
    _subs: Vec<Subscription>,
}

impl Sidebar {
    pub(crate) fn scroll_handle(&self) -> gpui_kit::ScrollHandle {
        self.list_scroll.base_handle().clone()
    }

    pub fn new(
        store: Entity<AppStore>,
        on_export: CompetitionAction,
        on_settings: CompetitionAction,
        on_preview: CompetitionAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let placeholder = cx.t("sidebar.search-placeholder");
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(placeholder));
        let subs = vec![
            cx.observe(&store, |this, _, cx| {
                this.reconcile(cx);
                this.rewatch_document(cx);
                cx.notify();
            }),
            cx.subscribe_in(
                &search,
                window,
                |_, _, event: &InputEvent, window, cx| match event {
                    InputEvent::Change => cx.notify(),
                    // Enter "applies" the filter by dropping focus.
                    InputEvent::PressEnter { .. } => window.blur(cx),
                    _ => {}
                },
            ),
        ];
        Self {
            store,
            search,
            selection: HashSet::new(),
            anchor: None,
            list_scroll: VirtualListScrollHandle::new(),
            context_menu: None,
            swipe: None,
            watched_doc: None,
            doc_sub: None,
            on_export,
            on_settings,
            on_preview,
            _subs: subs,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    /// Keeps [`Self::selection`] consistent with the store: drops competitions
    /// that no longer exist, and resets to the store's selection when that was
    /// changed from elsewhere (a new competition, an import, …).
    fn reconcile(&mut self, cx: &App) {
        let store = self.store.read(cx);
        self.selection.retain(|id| store.contains(*id));
        match store.selected_id() {
            Some(selected) if !self.selection.contains(&selected) => {
                self.selection.clear();
                self.selection.insert(selected);
                self.anchor = Some(selected);
            }
            None => self.selection.clear(),
            _ => {}
        }
    }

    /// Keeps [`Self::doc_sub`] on the selected document so its save-state
    /// changes (typing → saving → saved) repaint the row indicator.
    fn rewatch_document(&mut self, cx: &mut Context<Self>) {
        let doc = self.store.read(cx).selected_document().cloned();
        let id = doc.as_ref().map(|d| d.entity_id());
        if id != self.watched_doc {
            self.watched_doc = id;
            self.doc_sub = doc.map(|d| cx.observe(&d, |_, _, cx| cx.notify()));
        }
    }

    /// The listed competitions in display order, honouring the search filter.
    fn visible_ids(&self, cx: &App) -> Vec<Uuid> {
        let query = self.query(cx);
        self.store
            .read(cx)
            .summaries()
            .iter()
            .filter(|s| query.is_empty() || s.name.to_lowercase().contains(&query))
            .map(|s| s.id)
            .collect()
    }

    /// Apply a click on a competition row, honouring Cmd/Ctrl (toggle) and
    /// Shift (range).
    fn click_row(&mut self, id: Uuid, modifiers: gpui_kit::Modifiers, cx: &mut Context<Self>) {
        self.swipe = None;
        if modifiers.shift {
            let visible = self.visible_ids(cx);
            let from = self.anchor.and_then(|a| visible.iter().position(|v| *v == a));
            let to = visible.iter().position(|v| *v == id);
            if let (Some(from), Some(to)) = (from, to) {
                let (lo, hi) = (from.min(to), from.max(to));
                if !modifiers.secondary() {
                    self.selection.clear();
                }
                self.selection.extend(visible[lo..=hi].iter().copied());
                self.store.update(cx, |store, cx| store.select(id, cx));
                cx.notify();
                return;
            }
        }
        if modifiers.secondary() {
            if self.selection.contains(&id) {
                // Never empty the selection: the detail pane always shows one.
                if self.selection.len() > 1 {
                    self.selection.remove(&id);
                    let visible = self.visible_ids(cx);
                    let next = visible.into_iter().find(|v| self.selection.contains(v));
                    if let Some(next) = next {
                        self.anchor = Some(next);
                        self.store.update(cx, |store, cx| store.select(next, cx));
                    }
                }
            } else {
                self.selection.insert(id);
                self.anchor = Some(id);
                self.store.update(cx, |store, cx| store.select(id, cx));
            }
            cx.notify();
            return;
        }
        self.selection.clear();
        self.selection.insert(id);
        self.anchor = Some(id);
        self.store.update(cx, |store, cx| store.select(id, cx));
        cx.notify();
    }

    /// A right-click on a row outside the selection selects just that row
    /// first (Finder behaviour); inside it, the whole selection stays.
    fn prepare_context_menu(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.selection.contains(&id) {
            self.selection.clear();
            self.selection.insert(id);
            self.anchor = Some(id);
            self.store.update(cx, |store, cx| store.select(id, cx));
        }
    }

    /// Confirm, then move every selected competition to the trash — the
    /// File-menu "Löschen" and the context menu's entry.
    pub fn request_delete_selection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<Uuid> = self.selection.iter().copied().collect();
        self.confirm_and_delete(ids, window, cx);
    }

    /// Confirm, then delete every id in `ids` (`ids.len() == 1` picks the
    /// singular confirmation wording via `t_plural`).
    fn confirm_and_delete(&mut self, ids: Vec<Uuid>, window: &mut Window, cx: &mut Context<Self>) {
        if ids.is_empty() {
            return;
        }
        let message = cx.t_plural("sidebar.delete-confirm", ids.len() as i64, &[]);
        let detail = cx.t("sidebar.delete-confirm-detail");
        let delete_label = cx.t("sidebar.delete-confirm-delete-button");
        let cancel_label = cx.t("sidebar.delete-confirm-cancel-button");
        let answer = window.prompt(
            PromptLevel::Critical,
            &message,
            Some(&detail),
            &[
                gpui_kit::PromptButton::new(delete_label),
                gpui_kit::PromptButton::new(cancel_label),
            ],
            cx,
        );
        let store = self.store.clone();
        cx.spawn_in(window, async move |_, cx| {
            if answer.await.unwrap_or(1) != 0 {
                log::debug!("delete cancelled");
                return;
            }
            log::info!("deleting {} competition(s)", ids.len());
            cx.update(|_, cx| {
                store.update(cx, |store, cx| store.delete_competitions(ids, cx));
            })
            .ok();
        })
        .detach();
    }

    fn duplicate_one(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.duplicate_competition(id, cx));
    }

    /// Horizontal scrolling over a row pulls it sideways to reveal its actions; letting go settles
    /// it open or shut. Vertical scrolling is left to the list.
    fn swipe_scroll(&mut self, id: Uuid, event: &ScrollWheelEvent, cx: &mut Context<Self>) {
        let delta = event.delta.pixel_delta(px(20.));
        let (dx, dy): (f32, f32) = (delta.x.into(), delta.y.into());
        let current = match self.swipe {
            Some((swiped, offset)) if swiped == id => offset,
            _ => 0.,
        };
        if matches!(event.touch_phase, TouchPhase::Ended | TouchPhase::Cancelled) {
            let settled = if current > SWIPE_ACTIONS_W / 2. { SWIPE_ACTIONS_W } else { 0. };
            self.swipe = (settled > 0.).then_some((id, settled));
            cx.notify();
            return;
        }
        if dx.abs() <= dy.abs() {
            return;
        }
        let offset = (current - dx).clamp(0., SWIPE_ACTIONS_W + 24.);
        self.swipe = Some((id, offset));
        cx.stop_propagation();
        cx.notify();
    }

    fn open_context_menu(&mut self, id: Uuid, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some((id, position));
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The floating action menu for one competition row, anchored at the
    /// right-click's position — `None` unless it is currently open.
    fn context_menu_layer(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
        let (id, position) = self.context_menu?;
        let weak = cx.weak_entity();

        let on_dismiss: ContextMenuHandler = {
            let weak = weak.clone();
            Rc::new(move |_window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| this.close_context_menu(cx)).ok();
            })
        };

        let delete = ContextMenuItem::new("ctx-delete", cx.t("sidebar.context-delete"), Icon::Trash, {
            let weak = weak.clone();
            move |window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| this.request_delete_selection(window, cx))
                    .ok();
            }
        })
        .danger();
        // Everything but deleting acts on a single competition.
        let items = if self.selection.len() > 1 {
            vec![delete]
        } else {
        vec![
            ContextMenuItem::new("ctx-duplicate", cx.t("sidebar.context-duplicate"), Icon::Copy, {
                let weak = weak.clone();
                move |_window: &mut Window, cx: &mut App| {
                    weak.update(cx, |this, cx| this.duplicate_one(id, cx)).ok();
                }
            }),
            ContextMenuItem::new("ctx-export", cx.t("sidebar.context-export"), Icon::Export, {
                let on_export = self.on_export.clone();
                move |window: &mut Window, cx: &mut App| on_export(id, window, cx)
            }),
            ContextMenuItem::new(
                "ctx-settings",
                cx.t("sidebar.context-settings"),
                Icon::Settings,
                {
                    let on_settings = self.on_settings.clone();
                    move |window: &mut Window, cx: &mut App| on_settings(id, window, cx)
                },
            ),
            ContextMenuItem::new("ctx-preview", cx.t("sidebar.context-preview"), Icon::Preview, {
                let on_preview = self.on_preview.clone();
                move |window: &mut Window, cx: &mut App| on_preview(id, window, cx)
            }),
            delete.separated(),
        ]
        };

        Some(context_menu("sidebar-context-menu", position, items, on_dismiss, cx))
    }
}

enum Row {
    Year(i32),
    Competition {
        id: uuid::Uuid,
        name: String,
        meta: String,
    },
}

/// One virtual-list row. Height is forced to the `SB_*` constant the list's
/// `sizes` table declares for this index, so layout stays exact.
#[allow(clippy::too_many_arguments)]
fn sidebar_row(
    this: &Sidebar,
    idx: usize,
    row: &Row,
    c: crate::theme::PaletteColors,
    radius: Pixels,
    sel_fill: gpui_kit::Hsla,
    sel_fg: gpui_kit::Hsla,
    cx: &mut Context<Sidebar>,
) -> gpui_kit::AnyElement {
    match row {
        Row::Year(year) => div()
            .h(px(if idx == 0 { SB_YEAR_H_FIRST } else { SB_YEAR_H }))
            .w_full()
            .flex()
            .items_end()
            .px(px(10.))
            .pb(px(4.))
            .text_size(px(10.))
            .text_color(c.muted_foreground)
            .font_weight(FontWeight::BOLD)
            .child(year.to_string())
            .into_any_element(),
        Row::Competition { id, name, meta } => {
            let id = *id;
            let highlight = this.selection.contains(&id);
            let swipe_offset = match this.swipe {
                Some((swiped, offset)) if swiped == id => offset,
                _ => 0.,
            };
            let row = div()
                .id(id)
                .h(px(SB_COMP_H))
                .w_full()
                .flex()
                .items_center()
                .gap(px(8.))
                .mx(px(4.))
                .px(px(8.))
                .rounded(radius)
                .cursor_pointer()
                .when(highlight, |el| el.bg(sel_fill).text_color(sel_fg))
                .when(!highlight, |el| el.hover(|el| el.bg(c.surface)))
                .on_mouse_down(
                    MouseButton::Right,
                    cx.listener(move |this, event: &MouseDownEvent, _window, cx| {
                        this.prepare_context_menu(id, cx);
                        this.open_context_menu(id, event.position, cx);
                    }),
                )
                // Touch's right click: a long press.
                .on_aux_click(cx.listener(move |this, event: &ClickEvent, _window, cx| {
                    if matches!(event, ClickEvent::Touch(_)) && event.is_secondary() {
                        this.prepare_context_menu(id, cx);
                        this.open_context_menu(id, event.position(), cx);
                    }
                }))
                .on_click(cx.listener(move |this, ev: &ClickEvent, _window, cx| {
                    this.click_row(id, ev.modifiers(), cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .child(div().text_size(px(12.5)).truncate().child(name.clone()))
                        .child(
                            div()
                                .text_size(px(10.))
                                .text_color(if highlight {
                                    sel_fg
                                } else {
                                    c.muted_foreground
                                })
                                .child(meta.clone()),
                        ),
                )
                // Save state — only for the competition shown in the detail
                // pane, so it doesn't clutter every row.
                .children(
                    (this.store.read(cx).selected_id() == Some(id))
                        .then(|| save_indicator(this, c, cx)),
                );
            let action = |label: gpui_kit::SharedString, fill: gpui_kit::Hsla, text: gpui_kit::Hsla| {
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .h_full()
                    .w(px(SWIPE_ACTIONS_W / 2.))
                    .bg(fill)
                    .text_size(px(11.))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(text)
                    .child(label)
            };
            div()
                .id(gpui_kit::SharedString::from(format!("swipe-{id}")))
                .relative()
                .h(px(SB_COMP_H))
                .w_full()
                .overflow_hidden()
                .on_scroll_wheel(cx.listener(move |this, event: &ScrollWheelEvent, _window, cx| {
                    this.swipe_scroll(id, event, cx);
                }))
                .when(swipe_offset > 0., |el| {
                    el.child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .right_0()
                            .flex()
                            .child(
                                action(cx.t("sidebar.context-duplicate"), c.primary, c.primary_foreground)
                                    .id(gpui_kit::SharedString::from(format!("swipe-duplicate-{id}")))
                                    .on_click(cx.listener(move |this, _, _window, cx| {
                                        this.swipe = None;
                                        this.duplicate_one(id, cx);
                                    })),
                            )
                            .child(
                                action(cx.t("sidebar.context-delete"), c.critical, c.destructive_foreground)
                                    .id(gpui_kit::SharedString::from(format!("swipe-delete-{id}")))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.swipe = None;
                                        this.confirm_and_delete(vec![id], window, cx);
                                    })),
                            ),
                    )
                })
                .child(div().relative().left(px(-swipe_offset)).child(row))
                .into_any_element()
        }
    }
}

/// The sidebar's filter field. On the glass tier it sits on a glass capsule of
/// the same height as the "+" beside it (the field itself is bare); elsewhere
/// it is the usual bordered, filled pill.
fn search_capsule(
    search: &Entity<InputState>,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::AnyElement {
    use crate::components::toolbar_group::Bump;

    let native = glass::active();
    let bump = Bump::new("sidebar-search", window, cx);
    let field = Field::new("sidebar-search", search)
        .leading_icon(Icon::Search)
        .icon_scale(bump.scale)
        .pill();
    if native {
        // The probe sits on this unpadded wrapper (see `glass::region`).
        div()
            .relative()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .h(px(crate::components::toolbar_group::GROUP_HEIGHT))
            .rounded_full()
            .on_mouse_down(MouseButton::Left, bump.on_press())
            .child(glass::region_scaled(
                "sidebar-search",
                GlassRole::Capsule,
                bump.scale,
            ))
            .child(div().flex_1().min_w_0().child(field.bare()))
            .into_any_element()
    } else {
        // The macOS fallback tier draws no fill (just the field's own
        // outline, already drawn unconditionally) — Windows/Linux keep the
        // filled pill, their own distinct non-glass look.
        div()
            .flex_1()
            .min_w_0()
            .child(field.paints_background(!glass::mac_fallback()))
            .into_any_element()
    }
}

/// The small save-state marker at the end of the selected competition's row:
/// a spinner while saving (or an edit is pending), a red "!" on failure, each
/// with a tooltip — and nothing when all is well. The slot keeps its size
/// either way, so the title doesn't shift when the marker appears.
fn save_indicator(
    this: &Sidebar,
    c: crate::theme::PaletteColors,
    cx: &mut Context<Sidebar>,
) -> gpui_kit::AnyElement {
    use crate::components::spinner::Spinner;
    use crate::components::tooltip::text_tooltip;
    use crate::store::SaveState;

    let state = this
        .store
        .read(cx)
        .selected_document()
        .map(|doc| doc.read(cx).save_state())
        .unwrap_or(SaveState::Saved);
    let slot = || {
        div()
            .id("save-indicator")
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .size(px(16.))
    };
    let (tooltip, content): (gpui_kit::SharedString, gpui_kit::AnyElement) = match state {
        SaveState::Saved => return slot().into_any_element(),
        SaveState::Dirty => (
            cx.t("toolbar.save-state-dirty"),
            Spinner::new()
                .size(px(12.))
                .color(c.muted_foreground)
                .into_any_element(),
        ),
        SaveState::Saving => (
            cx.t("toolbar.save-state-saving"),
            Spinner::new()
                .size(px(12.))
                .color(c.muted_foreground)
                .into_any_element(),
        ),
        SaveState::Error(message) => (
            cx.t_fmt("sidebar.save-error-tooltip", &[("error", &message)])
                .into(),
            Icon::AlertCircle
                .size(px(14.))
                .color(c.critical)
                .into_any_element(),
        ),
    };
    slot()
        .tooltip(text_tooltip(tooltip))
        .child(content)
        .into_any_element()
}

impl Render for Sidebar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let radius = theme.skin.radius_lg_px();
        let (sel_fill, sel_fg) = sidebar_selection_fill(theme);
        let top_inset = titlebar::sidebar_top_inset(window, theme.skin.title_bar_height_px());

        let query = self.query(cx);

        // Year headers interleaved with matching competition rows (summaries are
        // already sorted newest-date-first, then by name).
        let mut rows: Vec<Row> = Vec::new();
        let mut current_year: Option<i32> = None;
        for summary in self.store.read(cx).summaries() {
            if !query.is_empty() && !summary.name.to_lowercase().contains(&query) {
                continue;
            }
            if current_year != Some(summary.year()) {
                current_year = Some(summary.year());
                rows.push(Row::Year(summary.year()));
            }
            rows.push(Row::Competition {
                id: summary.id,
                name: summary.name.clone(),
                meta: summary.date.format("%d.%m.%Y").to_string(),
            });
        }
        let is_empty = rows.is_empty();

        // Virtualised: only the visible rows are laid out. `sidebar_row` forces
        // each row to the height `sizes` declares here (`SB_*` constants).
        let sizes: Rc<Vec<Size<Pixels>>> = Rc::new(
            rows.iter()
                .enumerate()
                .map(|(idx, row)| {
                    let h = match row {
                        Row::Year(_) if idx == 0 => SB_YEAR_H_FIRST,
                        Row::Year(_) => SB_YEAR_H,
                        Row::Competition { .. } => SB_COMP_H,
                    };
                    size(px(1.), px(h))
                })
                .collect(),
        );
        let colors = *c;
        let list = div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(
                v_virtual_list(
                    cx.entity(),
                    "competition-list",
                    sizes,
                    move |this, range, _window, cx| {
                        range
                            .map(|idx| {
                                sidebar_row(
                                    this, idx, &rows[idx], colors, radius, sel_fill, sel_fg, cx,
                                )
                            })
                            .collect()
                    },
                )
                .track_scroll(&self.list_scroll)
                .pb(px(6.))
                .px(px(4.))
                .size_full(),
            )
            .child(Scrollbar::vertical(&self.list_scroll));

        div()
            .relative()
            .flex()
            .flex_col()
            // Width is owned by the enclosing resizable panel in `AppShell`.
            .size_full()
            // The native glass behind the sidebar (macOS 26+; a no-op elsewhere).
            .child(glass::region("sidebar", GlassRole::Sidebar))
            .bg(material::sidebar_fill(theme, cx))
            // Clears the macOS traffic lights (0 elsewhere) and, beside them,
            // holds the sidebar toggle.
            .when(top_inset > px(0.), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .h(top_inset)
                        .pl(titlebar::content_leading_inset(window))
                        // Same capsule (and id) as the collapsed-state toggle in
                        // the toolbar, so it's one glass view moving between them.
                        // The margin is *outside* the glass capsule.
                        .child(div().flex_none().mx(px(6.)).child(
                            ToolbarGroup::new("toolbar-toggle").chromeless().button(
                                Button::icon("sidebar-toggle", Icon::PanelLeft)
                                    .oval()
                                    .tooltip(cx.t("toolbar.toggle-sidebar"))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ToggleSidebar), cx)
                                    }),
                            ),
                        )),
                )
            })
            .when(is_empty, |el| {
                el.child(
                    div()
                        .flex_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(16.))
                        .text_size(px(12.))
                        .text_color(c.muted_foreground)
                        .child(if query.is_empty() {
                            cx.t("sidebar.empty-state")
                        } else {
                            cx.t("sidebar.no-matches")
                        }),
                )
            })
            .when(!is_empty, |el| el.child(list))
            .child(
                // Search + "add" live at the bottom (Xcode's navigator layout):
                // a capsule filter field and a round "+" beside it.
                div()
                    .id("sidebar-search-slot")
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(8.))
                    .p(px(8.))
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        let focused = this.search.read(cx).focus_handle(cx).is_focused(window);
                        if focused {
                            window.blur(cx);
                        }
                    }))
                    .child(search_capsule(&self.search, window, cx))
                    .child(
                        ToolbarGroup::new("sidebar-add").chromeless().button(
                            Button::icon("new-competition", Icon::Plus)
                                .round()
                                .tooltip(cx.t("sidebar.new-competition-button"))
                                .on_click(|_, window, cx| {
                                    window.dispatch_action(Box::new(NewCompetition), cx);
                                }),
                        ),
                    ),
            )
            .children(self.context_menu_layer(cx))
    }
}
