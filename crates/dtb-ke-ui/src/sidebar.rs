//! The navigation sidebar: a search field, then competitions grouped by year.
//!
//! A **multi-select mode** (toggled explicitly) replaces row selection with
//! checkboxes and a bulk-delete action.

use std::collections::HashSet;
use std::rc::Rc;

use gpui::FontWeight;
use gpui::{
    App, AppContext, Context, Entity, Focusable as _, InteractiveElement, IntoElement,
    MouseButton, MouseDownEvent, ParentElement, Pixels, Point, PromptLevel, Render, Size,
    StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
    size,
};
use gpui_base::input::{InputEvent, InputState};
use gpui_base::{Scrollbar, VirtualListScrollHandle, v_virtual_list};
use uuid::Uuid;

use crate::actions::file::NewCompetition;
use crate::components::button::{Button, ButtonTone};
use crate::components::context_menu::{ContextMenuHandler, ContextMenuItem, context_menu};
use crate::components::field::Field;
use crate::components::focus::selection_fill;
use crate::components::icon::Icon;
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

pub struct Sidebar {
    store: Entity<AppStore>,
    search: Entity<InputState>,
    /// Bulk-selection mode — entered / left explicitly via the toolbar buttons.
    multi_select: bool,
    checked: HashSet<Uuid>,
    list_scroll: VirtualListScrollHandle,
    /// The row context menu currently open, if any — the competition id and
    /// the window-absolute point (the right-click) to anchor the popover at.
    context_menu: Option<(Uuid, Point<Pixels>)>,
    on_export: CompetitionAction,
    on_settings: CompetitionAction,
    on_preview: CompetitionAction,
    _subs: Vec<Subscription>,
}

impl Sidebar {
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
            cx.observe(&store, |_, _, cx| cx.notify()),
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
            multi_select: false,
            checked: HashSet::new(),
            list_scroll: VirtualListScrollHandle::new(),
            context_menu: None,
            on_export,
            on_settings,
            on_preview,
            _subs: subs,
        }
    }

    fn query(&self, cx: &App) -> String {
        self.search.read(cx).value().trim().to_lowercase()
    }

    fn enter_multi_select(&mut self, cx: &mut Context<Self>) {
        self.multi_select = true;
        self.checked.clear();
        cx.notify();
    }

    fn exit_multi_select(&mut self, cx: &mut Context<Self>) {
        self.multi_select = false;
        self.checked.clear();
        cx.notify();
    }

    fn toggle_check(&mut self, id: Uuid, cx: &mut Context<Self>) {
        if !self.checked.remove(&id) {
            self.checked.insert(id);
        }
        cx.notify();
    }

    fn delete_checked(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let ids: Vec<Uuid> = self.checked.iter().copied().collect();
        self.confirm_and_delete(ids, window, cx);
    }

    fn delete_one(&mut self, id: Uuid, window: &mut Window, cx: &mut Context<Self>) {
        self.confirm_and_delete(vec![id], window, cx);
    }

    /// Confirm, then delete every id in `ids` — shared by the bulk-select
    /// toolbar and a single row's context menu (`ids.len() == 1` picks the
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
                gpui::PromptButton::new(delete_label),
                gpui::PromptButton::new(cancel_label),
            ],
            cx,
        );
        let store = self.store.clone();
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.unwrap_or(1) != 0 {
                log::debug!("delete cancelled");
                return;
            }
            log::info!("deleting {} competition(s)", ids.len());
            cx.update(|_, cx| {
                store.update(cx, |store, cx| {
                    for id in ids {
                        store.delete_competition(id, cx);
                    }
                });
            })
            .ok();
            this.update(cx, |this, cx| this.exit_multi_select(cx)).ok();
        })
        .detach();
    }

    fn duplicate_one(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.duplicate_competition(id, cx));
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
    fn context_menu_layer(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let (id, position) = self.context_menu?;
        let weak = cx.weak_entity();

        let on_dismiss: ContextMenuHandler = {
            let weak = weak.clone();
            Rc::new(move |_window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| this.close_context_menu(cx)).ok();
            })
        };

        let items = vec![
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
            ContextMenuItem::new("ctx-delete", cx.t("sidebar.context-delete"), Icon::Trash, {
                let weak = weak.clone();
                move |window: &mut Window, cx: &mut App| {
                    weak.update(cx, |this, cx| this.delete_one(id, window, cx))
                        .ok();
                }
            })
            .danger()
            .separated(),
        ];

        Some(context_menu("sidebar-context-menu", position, items, on_dismiss, cx))
    }

    fn select_toolbar(&self, cx: &Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let empty = self.store.read(cx).summaries().is_empty();

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(6.))
            .h(px(28.))
            .px(px(8.))
            .when(!self.multi_select, |el| {
                el.justify_end().when(!empty, |el| {
                    el.child(
                        Button::new("multi-enter", cx.t("sidebar.select-button"))
                            .tone(ButtonTone::Ghost)
                            .small()
                            .on_click(cx.listener(|this, _, _w, cx| this.enter_multi_select(cx))),
                    )
                })
            })
            .when(self.multi_select, |el| {
                el.child(
                    Button::new("multi-done", cx.t("sidebar.done-button"))
                        .tone(ButtonTone::Ghost)
                        .small()
                        .on_click(cx.listener(|this, _, _w, cx| this.exit_multi_select(cx))),
                )
                .child(
                    div()
                        .flex_1()
                        .text_size(px(11.))
                        .text_color(c.muted_foreground)
                        .child(cx.t_fmt(
                            "sidebar.selected-count",
                            &[("n", &self.checked.len().to_string())],
                        )),
                )
                .child(
                    Button::new("multi-delete", cx.t("sidebar.delete-button"))
                        .tone(ButtonTone::Danger)
                        .small()
                        .disabled(self.checked.is_empty())
                        .on_click(
                            cx.listener(|this, _, window, cx| this.delete_checked(window, cx)),
                        ),
                )
            })
    }

    fn status_bar(&self, cx: &App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let count = self.store.read(cx).summaries().len();
        let status = self.store.read(cx).status();

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(px(4.))
            .h(px(22.))
            .px(px(10.))
            .py(px(4.))
            .text_size(px(10.5))
            .text_color(c.muted_foreground)
            .child(div().w_1().h_1().block().rounded_full().bg(match status {
                crate::store::Status::Connecting => theme.color.warn,
                crate::store::Status::Ready => theme.color.ok,
                crate::store::Status::Failed(_) => theme.color.critical,
            }))
            .child(cx.t_plural("sidebar.competition-count", count as i64, &[]))
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
    sel_fill: gpui::Hsla,
    sel_fg: gpui::Hsla,
    multi: bool,
    selected: Option<Uuid>,
    cx: &mut Context<Sidebar>,
) -> gpui::AnyElement {
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
            let checked = this.checked.contains(&id);
            let is_selected = !multi && selected == Some(id);
            let highlight = is_selected || (multi && checked);
            div()
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
                        this.open_context_menu(id, event.position, cx);
                    }),
                )
                .on_click(cx.listener(move |this, _, _window, cx| {
                    if this.multi_select {
                        this.toggle_check(id, cx);
                    } else {
                        this.store.update(cx, |store, cx| store.select(id, cx));
                    }
                }))
                .when(multi, |el| {
                    el.child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .justify_center()
                            .size(px(15.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if checked { c.primary } else { c.line_strong })
                            .when(checked, |el| el.bg(c.primary))
                            .when(checked, |el| {
                                el.child(Icon::Check.size(px(11.)).color(c.primary_foreground))
                            }),
                    )
                })
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
                .into_any_element()
        }
    }
}

impl Render for Sidebar {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let radius = theme.skin.radius_control_px();
        let (sel_fill, sel_fg) = selection_fill(theme);
        let multi = self.multi_select;

        let selected = self.store.read(cx).selected_id();
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
                                    this, idx, &rows[idx], colors, radius, sel_fill, sel_fg, multi,
                                    selected, cx,
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
            .flex()
            .flex_col()
            // Width is owned by the enclosing resizable panel in `AppShell`.
            .size_full()
            .bg(material::sidebar_fill(theme, cx))
            .border_r_1()
            .border_color(c.border)
            .child(
                div()
                    .id("sidebar-search-slot")
                    .p(px(8.))
                    .on_mouse_down_out(cx.listener(|this, _, window, cx| {
                        let focused = this.search.read(cx).focus_handle(cx).is_focused(window);
                        if focused {
                            window.blur(cx);
                        }
                    }))
                    .child(
                        Field::new("sidebar-search", &self.search)
                            .leading_icon(Icon::Search)
                            .paints_background(false),
                    ),
            )
            .child(self.select_toolbar(cx))
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
                div()
                    .pt(px(8.))
                    .px(px(8.))
                    .border_t_1()
                    .border_color(c.border)
                    .child(
                        Button::new("new-competition", cx.t("sidebar.new-competition-button"))
                            .tone(ButtonTone::Secondary)
                            .leading_icon(Icon::Plus)
                            .on_click(|_, window, cx| {
                                window.dispatch_action(Box::new(NewCompetition), cx);
                            }),
                    ),
            )
            .child(self.status_bar(cx))
            .children(self.context_menu_layer(cx))
    }
}
