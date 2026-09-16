//! The trash window — lists soft-deleted competitions
//! ([`AppStore::delete_competition`]/`delete_competitions`, see `store.rs`)
//! with a restore action per row, a permanent-delete action per row, and an
//! "empty trash" action for the actually-irreversible step. A separate
//! window rather than a sidebar mode: a trashed competition can't be edited
//! without restoring it first, so it would be a mis-click risk to reuse the
//! sidebar's own "click a row to select and edit it" affordance for it.

use std::cell::RefCell;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, PromptLevel, Render, ScrollHandle,
    SharedString, Size, StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window,
    WindowBounds, WindowKind, WindowOptions, div, prelude::FluentBuilder, px,
};
use gpui_base::Scrollbar;
use uuid::Uuid;

use crate::components::icon::Icon;
use crate::components::{Button, ButtonTone};
use crate::i18n::ActiveLocale;
use crate::store::AppStore;
use crate::theme::{ActiveTheme, PaletteColors};

thread_local! {
    static OPEN: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
}

/// Open (or focus) the trash window.
pub fn open(store: Entity<AppStore>, cx: &mut App) {
    // Existence via `cx.windows()`, not `handle.update` — see `about::open_kind`.
    let existing = OPEN.with(|h| *h.borrow());
    if let Some(handle) = existing {
        if cx.windows().contains(&handle) {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            return;
        }
        OPEN.with(|h| *h.borrow_mut() = None);
    }

    let options = window_options(cx);
    match cx.open_window(options, |window, cx| cx.new(|cx| TrashWindow::new(store, window, cx))) {
        Ok(handle) => OPEN.with(|h| *h.borrow_mut() = Some(handle.into())),
        Err(err) => log::error!("failed to open the trash window: {err}"),
    }
}

fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(520.), px(480.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(cx.t("trash.window-title")),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_minimizable: true,
        window_min_size: Some(Size::new(px(420.), px(340.))),
        ..Default::default()
    }
}

pub struct TrashWindow {
    store: Entity<AppStore>,
    focus: FocusHandle,
    scroll: ScrollHandle,
    _store_sub: Subscription,
}

impl Drop for TrashWindow {
    fn drop(&mut self) {
        OPEN.with(|h| *h.borrow_mut() = None);
    }
}

impl TrashWindow {
    fn new(store: Entity<AppStore>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        Self {
            store,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            _store_sub: store_sub,
        }
    }

    fn restore(&mut self, id: Uuid, cx: &mut Context<Self>) {
        self.store
            .update(cx, |store, cx| store.restore_competition(id, cx));
    }

    fn purge_one(&mut self, id: Uuid, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let title = cx.t_fmt("trash.purge-confirm", &[("name", &name)]);
        let detail = cx.t("trash.purge-confirm-detail");
        let delete_label = cx.t("trash.purge-confirm-delete-button");
        let cancel_label = cx.t("trash.purge-confirm-cancel-button");
        let answer = window.prompt(
            PromptLevel::Critical,
            &title,
            Some(&detail),
            &[
                gpui::PromptButton::new(delete_label),
                gpui::PromptButton::new(cancel_label),
            ],
            cx,
        );
        let store = self.store.clone();
        cx.spawn_in(window, async move |_, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            cx.update(|_, cx| {
                store.update(cx, |store, cx| store.purge_competition(id, cx));
            })
            .ok();
        })
        .detach();
    }

    fn empty_trash(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let count = self.store.read(cx).trashed().len();
        if count == 0 {
            return;
        }
        let title = cx.t_plural("trash.empty-confirm", count as i64, &[]);
        let detail = cx.t("trash.empty-confirm-detail");
        let empty_label = cx.t("trash.empty-confirm-button");
        let cancel_label = cx.t("trash.empty-confirm-cancel-button");
        let answer = window.prompt(
            PromptLevel::Critical,
            &title,
            Some(&detail),
            &[
                gpui::PromptButton::new(empty_label),
                gpui::PromptButton::new(cancel_label),
            ],
            cx,
        );
        let store = self.store.clone();
        cx.spawn_in(window, async move |_, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            cx.update(|_, cx| {
                store.update(cx, |store, cx| store.purge_all_trashed(cx));
            })
            .ok();
        })
        .detach();
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let has_trashed = !self.store.read(cx).trashed().is_empty();

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .h(px(44.))
            .border_b_1()
            .border_color(c.border)
            .child(div().flex_1())
            .child(
                Button::new("empty-trash", cx.t("trash.empty-trash-button"))
                    .small()
                    .tone(ButtonTone::Danger)
                    .disabled(!has_trashed)
                    .on_click(cx.listener(|this, _, window, cx| this.empty_trash(window, cx))),
            )
    }

    fn row(&self, id: Uuid, name: &str, meta: String, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let name_owned = name.to_string();

        div()
            .id(format!("trash-row-{id}"))
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(10.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(div().text_size(px(13.)).truncate().child(name.to_string()))
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(c.muted_foreground)
                            .child(meta),
                    ),
            )
            .child(
                Button::new(format!("restore-{id}"), cx.t("trash.restore-button"))
                    .small()
                    .tone(ButtonTone::Secondary)
                    .leading_icon(Icon::RotateCw)
                    .on_click(cx.listener(move |this, _, _window, cx| this.restore(id, cx))),
            )
            .child(
                div()
                    .id(format!("purge-{id}"))
                    .flex()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(radius)
                    .cursor_pointer()
                    .text_color(c.critical)
                    .hover(|el| {
                        el.bg(gpui::Hsla {
                            a: 0.12,
                            ..c.critical
                        })
                    })
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.purge_one(id, name_owned.clone(), window, cx)
                    }))
                    .child(Icon::Trash.size(px(14.))),
            )
    }
}

impl Render for TrashWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let font = cx.theme().skin.font_family();
        let lead = crate::skin::titlebar::content_leading_inset(window);

        if window.is_window_active() && window.focused(cx).is_none() {
            let handle = self.focus.clone();
            window.focus(&handle, cx);
        }

        let trashed = self.store.read(cx).trashed().to_vec();
        let is_empty = trashed.is_empty();
        let mut rows = Vec::with_capacity(trashed.len());
        for t in &trashed {
            let meta = row_meta(t, cx);
            rows.push(self.row(t.id, &t.name, meta, cx).into_any_element());
        }

        div()
            .track_focus(&self.focus)
            .key_context("TrashWindow")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .when_some(font, |el, family| el.font_family(family))
            .child(
                div()
                    .flex_none()
                    .h(px(34.))
                    .pl(lead)
                    .pr(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(c.border)
                    .bg(c.chrome)
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(cx.t("trash.window-title")),
            )
            .child(self.toolbar(cx))
            .when(is_empty, |el| {
                el.child(empty_state(cx.t("trash.empty-state"), &c))
            })
            .when(!is_empty, |el| {
                el.child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(
                            div()
                                .id("trash-list")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.scroll)
                                .children(rows),
                        )
                        .child(Scrollbar::vertical(&self.scroll)),
                )
            })
    }
}

impl Focusable for TrashWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

fn empty_state(text: impl Into<SharedString>, c: &PaletteColors) -> gpui::AnyElement {
    div()
        .flex_1()
        .flex()
        .items_center()
        .justify_center()
        .p(px(24.))
        .text_size(px(13.))
        .text_color(c.muted_foreground)
        .child(text.into())
        .into_any_element()
}

fn row_meta(t: &dtb_ke_persist::TrashedCompetition, cx: &Context<TrashWindow>) -> String {
    let deleted_local: chrono::DateTime<chrono::Local> = t.deleted_at.with_timezone(&chrono::Local);
    let date = t.date.format("%d.%m.%Y").to_string();
    let deleted_on = cx.t_fmt(
        "trash.deleted-on",
        &[("date", &deleted_local.format("%d.%m.%Y").to_string())],
    );
    format!("{date} · {deleted_on}")
}
