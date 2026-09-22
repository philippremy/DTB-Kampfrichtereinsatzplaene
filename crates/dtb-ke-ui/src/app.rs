//! The application shell: window frame + title bar + [sidebar | detail] + status
//! strip. Owns the root [`AppStore`] and the sidebar / toolbar entities.

use std::cell::RefCell;
use std::sync::Arc;
use std::time::Duration;

use dtb_ke_export::Exporter;
use futures::channel::oneshot;
use gpui::{
    Animation, AnimationExt, AnyWindowHandle, App, AppContext, ClickEvent, Context, Entity,
    FocusHandle, InteractiveElement, IntoElement, MouseButton, ParentElement, PathPromptOptions,
    PromptLevel, Render, StatefulInteractiveElement, Styled, Subscription, WeakEntity, Window,
    WindowControlArea, WindowHandle, div, ease_out_quint, prelude::FluentBuilder, px,
};
use gpui_base::{ResizableState, h_resizable, resizable_panel};
use log::{debug, error, info, warn};
use uuid::Uuid;

use crate::actions::app::CheckForUpdates;
use crate::actions::edit::{DeleteJudgingTable, DuplicateJudgingTable, Redo, Undo};
use crate::actions::file::{
    AddJudgingTable, CloseWindow, CompetitionSettings, DeleteCompetitions, ExportAll,
    ExportCompetition, ImportCompetition, NewCompetition, ShowTrash,
};
use crate::actions::window::{Minimize, ToggleFullscreen, TogglePreview, ToggleSidebar};
use crate::components::button::Button;
use crate::components::icon::Icon;
use crate::components::menu_bar::MenuBar;
use crate::components::toolbar_group::ToolbarGroup;
use crate::detail::DetailView;
use crate::i18n::{ActiveLocale, Locale};
use crate::material;
use crate::menu::{self, DbStatus, MenuState};
use crate::preview::{self, PreviewWindow};
use crate::save::{self, ExportFormat, SaveChoice};
use crate::sidebar::{SIDEBAR_MAX, SIDEBAR_MIN, SIDEBAR_SNAP, SIDEBAR_WIDTH, Sidebar};
use crate::skin::glass::{self, GlassRole};
use crate::skin::{decorations, menu as skin_menu, titlebar, window as skin_window};
use crate::store::{self, AppStore};
use crate::theme::{ActiveTheme, Appearance, Theme};
use crate::toolbar::CompetitionToolbar;
use crate::updater::{self, Updater, UpdaterEvent, UpdaterToast};

/// Select `id` in `store` — which may not be open in memory yet, so this can
/// take a moment (see `AppStore::select_then`) — then invoke `act` on
/// `AppShell` once it becomes the active selection. Used by the sidebar's
/// context menu: dispatching the equivalent `window` action right after
/// calling `select` would race the *previous* frame's action-dispatch tree
/// (rebuilt only on the next repaint), so this calls the handler directly
/// instead once the data is actually ready.
fn select_then_act(
    store: Entity<AppStore>,
    weak: WeakEntity<AppShell>,
    id: Uuid,
    window: &mut Window,
    cx: &mut App,
    act: impl FnOnce(&mut AppShell, &mut Window, &mut Context<AppShell>) + 'static,
) {
    let (tx, rx) = oneshot::channel::<()>();
    store.update(cx, |store, cx| {
        store.select_then(id, cx, move |_cx| {
            let _ = tx.send(());
        });
    });
    window
        .spawn(cx, async move |cx| {
            if rx.await.is_ok() {
                cx.update(|window, cx| {
                    weak.update(cx, |this, cx| act(this, window, cx)).ok();
                })
                .ok();
            }
        })
        .detach();
}

pub struct AppShell {
    store: Entity<AppStore>,
    sidebar: Entity<Sidebar>,
    toolbar: Entity<CompetitionToolbar>,
    detail: Entity<DetailView>,
    /// The in-app menu bar (only rendered on `InApp` skins — Windows, Linux).
    menu_bar: Entity<MenuBar>,
    /// Focused whenever nothing else in the window is, so the shell's menu-command
    /// action handlers stay on the dispatch path (macOS greys a menu item whose
    /// action isn't reachable from the focused node).
    focus_handle: FocusHandle,
    sidebar_collapsed: bool,
    /// Current / last-good sidebar width; restored when it is re-opened.
    sidebar_width: gpui::Pixels,
    /// Drives the `[sidebar | detail]` split drag.
    resizable_state: Entity<ResizableState>,
    /// CSD only: a left-button press landed on the title bar; the next move
    /// starts a window drag (so a plain click on a title-bar control doesn't).
    drag_armed: bool,
    /// Last value pushed to the platform window's "edited" flag (macOS document
    /// dot); avoids re-calling it every render.
    window_edited: bool,
    /// Last menu state installed; the bar is only rebuilt when this changes.
    last_menu_state: Option<MenuState>,
    /// The Typst preview, when open. Constructed lazily (`Exporter::new` decodes
    /// fonts). `_preview_release` clears `preview_window` if the user closes it.
    exporter: Option<Arc<Exporter>>,
    preview_window: Option<WindowHandle<PreviewWindow>>,
    _preview_release: Option<Subscription>,
    /// Re-render `AppShell` when the store's aggregate dirty flag changes.
    _store_sub: Subscription,
    /// Re-render when the detail view's table selection changes (menu state).
    _detail_sub: Subscription,
    /// Keeps the theme's OS-appearance side in step with the window.
    _appearance_sub: Subscription,
    /// Rebuilds the menu bar (native + in-app) when the active locale changes
    /// — `cx.refresh_windows()` alone re-renders gpui views but not the
    /// push-based native macOS menu.
    _locale_sub: Subscription,
    /// The in-app updater — background checks + the corner toast.
    updater: Entity<Updater>,
    _updater_sub: Subscription,
    /// The startup delay + periodic recheck loop.
    _updater_loop: gpui::Task<()>,
    /// The startup delay + periodic recheck loop for `AppStore::maybe_backup`
    /// — same shape as `_updater_loop`, see its construction below.
    _backup_loop: gpui::Task<()>,
}

impl AppShell {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let store = AppStore::new(cx);

        // The sidebar's context menu reuses these three handlers (rather than
        // going through `window.dispatch_action`, which would race the
        // selection this closure makes — see `select_then_act`) so "Export …"
        // / "Wettkampfeinstellungen …" / "Vorschau" work for a competition the
        // user hasn't already clicked into.
        let on_export: crate::sidebar::CompetitionAction = {
            let weak = cx.weak_entity();
            let store = store.clone();
            std::rc::Rc::new(move |id, window, cx| {
                select_then_act(store.clone(), weak.clone(), id, window, cx, |this, window, cx| {
                    this.handle_export(&ExportCompetition, window, cx);
                });
            })
        };
        let on_settings: crate::sidebar::CompetitionAction = {
            let weak = cx.weak_entity();
            let store = store.clone();
            std::rc::Rc::new(move |id, window, cx| {
                select_then_act(store.clone(), weak.clone(), id, window, cx, |this, window, cx| {
                    this.detail
                        .update(cx, |detail, cx| detail.open_meta_dialog(window, cx));
                });
            })
        };
        let on_preview: crate::sidebar::CompetitionAction = {
            let weak = cx.weak_entity();
            let store = store.clone();
            std::rc::Rc::new(move |id, window, cx| {
                select_then_act(store.clone(), weak.clone(), id, window, cx, |this, _window, cx| {
                    this.toggle_preview(cx);
                });
            })
        };
        let sidebar =
            cx.new(|cx| Sidebar::new(store.clone(), on_export, on_settings, on_preview, window, cx));
        let detail = cx.new(|cx| DetailView::new(store.clone(), window, cx));
        let toolbar = cx.new(|cx| CompetitionToolbar::new(store.clone(), detail.clone(), cx));
        let menu_bar = cx.new(|_| MenuBar::new());
        let store_sub = cx.observe(&store, |_, _, cx| cx.notify());
        let detail_sub = cx.observe(&detail, |_, _, cx| cx.notify());

        let updater = cx.new(|_| Updater::new());
        let updater_sub = cx.subscribe(&updater, |this, _, event: &UpdaterEvent, cx| match event {
            UpdaterEvent::Changed => cx.notify(),
            UpdaterEvent::RelaunchRequested => this.finish_update(cx),
        });
        // First check after a short settle delay, then poll on the recheck
        // interval (each `check` is itself throttled + gated on the setting).
        let updater_loop = {
            let updater = updater.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(updater::STARTUP_DELAY).await;
                loop {
                    updater.update(cx, |u, cx| u.check(false, cx));
                    cx.background_executor()
                        .timer(updater::RECHECK_INTERVAL)
                        .await;
                }
            })
        };

        // Same shape as `updater_loop` above: `AppStore::maybe_backup` is
        // cheap to call repeatedly (its own already-backed-up-today /
        // setting-off checks make it a no-op almost every time), so a
        // startup delay then a long recheck interval is enough to catch a
        // fresh day without a dedicated "wait until midnight" timer.
        let backup_loop = {
            let store = store.clone();
            cx.spawn(async move |_, cx| {
                cx.background_executor().timer(store::BACKUP_STARTUP_DELAY).await;
                loop {
                    store.update(cx, |store, cx| store.maybe_backup(cx));
                    cx.background_executor()
                        .timer(store::BACKUP_RECHECK_INTERVAL)
                        .await;
                }
            })
        };

        // Startup value + follow OS light/dark changes.
        Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        let appearance_sub = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        });

        // Locale changed (Settings picker, or live OS follow later) — rebuild
        // the menu bar with the last-known state (translation-independent;
        // no `&Window` available here for a fresh `sync_menus`).
        let locale_sub = cx.observe_global::<Locale>(|this, cx| {
            if let Some(state) = this.last_menu_state {
                skin_menu::install(state, cx);
                if skin_menu::in_app(cx) {
                    this.menu_bar.update(cx, |bar, cx| {
                        let menus = menu::build(state, cx.global::<Locale>());
                        bar.set_menus(menus, cx);
                    });
                }
            }
            cx.notify();
        });

        // Closing the main window on macOS doesn't quit the app (so the
        // per-document `on_app_quit` flush won't run) — flush any pending edits
        // while the store is still alive. (The `MAIN_WINDOW` handle is cleared
        // in `Drop`, on the actual teardown.)
        window.on_window_should_close(cx, {
            let store = store.clone();
            move |_window, cx| {
                store.update(cx, |store, cx| store.flush_all(cx)).detach();
                true
            }
        });

        Self {
            store,
            sidebar,
            toolbar,
            detail,
            menu_bar,
            focus_handle: cx.focus_handle(),
            sidebar_collapsed: false,
            sidebar_width: SIDEBAR_WIDTH,
            resizable_state: cx.new(|_| ResizableState::default()),
            drag_armed: false,
            window_edited: false,
            last_menu_state: None,
            exporter: None,
            preview_window: None,
            _preview_release: None,
            _store_sub: store_sub,
            _detail_sub: detail_sub,
            _appearance_sub: appearance_sub,
            _locale_sub: locale_sub,
            updater,
            _updater_sub: updater_sub,
            _updater_loop: updater_loop,
            _backup_loop: backup_loop,
        }
    }
}

impl Drop for AppShell {
    fn drop(&mut self) {
        // The main window is gone — let `open_main_window` (and the macOS dock
        // `on_reopen`) build a fresh one instead of trying to focus this.
        MAIN_WINDOW.with(|m| *m.borrow_mut() = None);
    }
}

impl AppShell {
    /// The corner update toast, `deferred` so it floats over everything.
    fn updater_toast(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        if !updater::available() || !self.updater.read(cx).toast_visible() {
            return None;
        }
        let state = self.updater.read(cx).state().clone();
        let expanded = self.updater.read(cx).expanded();
        let can_self_install =
            updater::can_self_install(crate::settings::Settings::global(cx).update_channel);
        let u = self.updater.downgrade();

        let bind = |f: fn(&mut Updater, &mut Context<Updater>)| {
            let u = u.clone();
            move |_: &mut Window, cx: &mut App| {
                u.update(cx, f).ok();
            }
        };
        let open_notes = {
            let u = u.clone();
            move |_: &mut Window, cx: &mut App| {
                if let Ok(url) = u.update(cx, |u, _| u.release_page()) {
                    cx.open_url(&url);
                }
            }
        };

        let toast = UpdaterToast::new(state, expanded, can_self_install)
            .on_toggle(bind(Updater::toggle_expanded))
            .on_skip(bind(Updater::skip))
            .on_later(bind(Updater::snooze))
            .on_primary(bind(Updater::primary_action))
            .on_dismiss(bind(Updater::dismiss))
            .on_outside_click(bind(Updater::collapse))
            .on_notes(open_notes);

        Some(
            gpui::deferred(toast)
                .with_priority(gpui_base::POPUP_PRIORITY)
                .into_any_element(),
        )
    }

    /// The updater has swapped the files in place — flush every open document,
    /// then re-exec the (now updated) binary.
    fn finish_update(&mut self, cx: &mut Context<Self>) {
        let store = self.store.clone();
        cx.spawn(async move |_, cx| {
            store.update(cx, |store, cx| store.flush_all(cx)).await;
            log::info!("relaunching into the updated build");
            // `restart()` only returns on failure (`Ok` is `Infallible`).
            let Err(err) = self_update::restart::restart();
            log::error!("relaunch into the updated build failed: {err}");
        })
        .detach();
    }

    /// Push the aggregate unsaved state to the platform window (macOS document
    /// dot; a no-op elsewhere).
    fn sync_window_edited(&mut self, window: &mut Window, cx: &App) {
        let edited = self.store.read(cx).any_document_unsaved();
        if self.window_edited != edited {
            self.window_edited = edited;
            window.set_window_edited(edited);
        }
    }

    /// Rebuild the menu bar when the state that decides its labels / enabled
    /// items changes.
    fn sync_menus(&mut self, window: &Window, cx: &mut App) {
        let (can_undo, can_redo) = self.store.read(cx).selected_undo_state();
        let state = MenuState {
            document_context: true,
            has_competition: self.store.read(cx).selected_id().is_some(),
            has_table_selection: self.detail.read(cx).has_table_selection(),
            can_undo,
            can_redo,
            fullscreen: window.is_fullscreen(),
            preview_open: self.preview_window.is_some(),
            db_status: match self.store.read(cx).status() {
                store::Status::Connecting => DbStatus::Connecting,
                store::Status::Ready => DbStatus::Ready,
                store::Status::Failed(_) => DbStatus::Failed,
            },
            competition_count: self.store.read(cx).summaries().len(),
        };
        if self.last_menu_state != Some(state) {
            self.last_menu_state = Some(state);
            skin_menu::install(state, cx);
            if skin_menu::in_app(cx) {
                self.menu_bar.update(cx, |bar, cx| {
                    let menus = menu::build(state, cx.global::<Locale>());
                    bar.set_menus(menus, cx);
                });
            }
        }
    }

    /// The shared Typst exporter, built on first use (`Exporter::new` decodes
    /// the embedded fonts, so we don't pay that at startup).
    fn exporter(&mut self) -> Option<Arc<Exporter>> {
        if self.exporter.is_none() {
            match Exporter::new() {
                Ok(exporter) => {
                    debug!("shared Typst exporter constructed");
                    self.exporter = Some(Arc::new(exporter));
                }
                Err(err) => {
                    error!("could not construct the Typst exporter: {err}");
                    return None;
                }
            }
        }
        self.exporter.clone()
    }

    /// Open the preview window; if it's already open but hidden behind another
    /// window bring it forward; if it's already frontmost close it (toggle).
    fn toggle_preview(&mut self, cx: &mut Context<Self>) {
        if let Some(handle) = self.preview_window {
            let frontmost = cx.active_window() == Some(handle.into());
            if !frontmost {
                handle
                    .update(cx, |_, window, _| window.activate_window())
                    .ok();
                return;
            }
        }
        if let Some(handle) = self.preview_window.take() {
            debug!("closing the preview window");
            if handle
                .update(cx, |_, window, _| window.remove_window())
                .is_err()
            {
                warn!("preview window handle was already gone");
            }
            self._preview_release = None;
            cx.notify();
            return;
        }
        debug!("opening the preview window");

        let Some(exporter) = self.exporter() else {
            return;
        };
        let store = self.store.clone();

        let options = preview::window_options(cx);
        let handle = match cx.open_window(options, |_, cx| {
            cx.new(|cx| PreviewWindow::new(store, exporter, cx))
        }) {
            Ok(handle) => handle,
            Err(err) => {
                log::error!("Vorschau-Fenster konnte nicht geöffnet werden: {err}");
                return;
            }
        };

        // Clear our handle when the window is closed from its own title bar.
        if let Ok(entity) = handle.entity(cx) {
            self._preview_release = Some(cx.observe_release(&entity, |this, _, cx| {
                this.preview_window = None;
                this._preview_release = None;
                cx.notify();
            }));
        }
        self.preview_window = Some(handle);
        cx.notify();
    }

    fn handle_export(
        &mut self,
        _: &ExportCompetition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(id) = self.store.read(cx).selected_id() else {
            return;
        };
        let name = self
            .store
            .read(cx)
            .name_of(id)
            .map(str::to_owned)
            .unwrap_or_else(|| cx.t("app.default-competition-name").to_string());

        info!("export requested for competition {id} (\"{name}\")");
        let receiver = save::prompt(name, cx);
        let store = self.store.clone();
        let exporter = self.exporter();

        cx.spawn_in(window, async move |_, cx| {
            let Ok(Some(SaveChoice { path, format })) = receiver.await else {
                debug!("export dialog cancelled");
                return;
            };
            let fmt = match &format {
                ExportFormat::Pdf(_) => "PDF",
                ExportFormat::Docx(_) => "DOCX",
                ExportFormat::Blob => ".dtbke",
            };
            info!("exporting {id} as {fmt} → {}", path.display());

            match format {
                ExportFormat::Blob => {
                    store.update(cx, |store, cx| store.export_competition(id, path, cx));
                }
                pdf_or_docx => {
                    let Some(exporter) = exporter else {
                        error!("export aborted — the Typst exporter is unavailable");
                        alert_t(cx, "app.exporter-unavailable");
                        return;
                    };
                    let Some(dto) = store.update(cx, |store, cx| {
                        store
                            .selected_document()
                            .cloned()
                            .map(|doc| doc.read(cx).to_dto(cx))
                    }) else {
                        return;
                    };

                    let target = path.clone();
                    let compiled = cx
                        .background_executor()
                        .spawn(async move {
                            let bytes = match &pdf_or_docx {
                                ExportFormat::Pdf(o) => exporter.compile_pdf(&dto, o),
                                ExportFormat::Docx(o) => exporter.compile_docx(&dto, o),
                                ExportFormat::Blob => unreachable!(),
                            };
                            match bytes {
                                Ok(bytes) => {
                                    let n = bytes.len();
                                    std::fs::write(&path, bytes)
                                        .map(|()| n)
                                        .map_err(|e| WriteError::Io(e.to_string()))
                                }
                                Err(err) => Err(WriteError::Compile(err.to_string())),
                            }
                        })
                        .await;

                    match compiled {
                        Ok(n) => info!("{fmt} export written — {n} bytes → {}", target.display()),
                        Err(err) => {
                            error!("{fmt} export failed: {err:?}");
                            let detail = err.detail(cx);
                            alert_t_fmt(cx, "app.export-failed", &[("detail", &detail)]);
                        }
                    }
                }
            }
        })
        .detach();
    }

    fn handle_export_all(&mut self, _: &ExportAll, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_new_path(&default_dir(), Some("PersistedSessions.bin"));
        let store = self.store.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(dest))) = receiver.await else {
                debug!("full-database export cancelled");
                return;
            };
            info!("full-database export → {}", dest.display());
            store.update(cx, |store, cx| store.export_all(dest, cx));
        })
        .detach();
    }

    fn handle_show_trash(&mut self, _: &ShowTrash, _window: &mut Window, cx: &mut Context<Self>) {
        crate::trash_window::open(self.store.clone(), cx);
    }

    fn handle_import(
        &mut self,
        _: &ImportCompetition,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let prompt = cx.t("app.import-prompt");
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some(prompt),
        });
        let store = self.store.clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                debug!("import cancelled");
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            info!("importing competition from {}", path.display());
            let bytes = match std::fs::read(&path) {
                Ok(bytes) => bytes,
                Err(err) => {
                    warn!("import: {} not readable: {err}", path.display());
                    return;
                }
            };
            let dto = match dtb_ke_persist::competition_dto_from_bytes(&bytes) {
                Ok(dto) => dto,
                Err(err) => {
                    warn!("import: invalid file {}: {err}", path.display());
                    return;
                }
            };

            let id = dto.id;
            let exists = store.update(cx, |store, _| store.contains(id));
            if exists {
                let confirm = cx.update(|window, cx| {
                    let detail = cx.t("app.import-overwrite-detail");
                    window.prompt(
                        PromptLevel::Warning,
                        &cx.t("app.import-overwrite-confirm"),
                        Some(&detail),
                        &[
                            gpui::PromptButton::new(cx.t("app.import-overwrite-button")),
                            gpui::PromptButton::new(cx.t("app.import-overwrite-cancel-button")),
                        ],
                        cx,
                    )
                });
                let Ok(confirm) = confirm else { return };
                if confirm.await.unwrap_or(1) != 0 {
                    debug!("import of {id} declined at the overwrite prompt");
                    return;
                }
            }
            store.update(cx, |store, cx| store.import_decoded(dto, cx));
        })
        .detach();
    }

    /// The toolbar band at the top of the **content column** (the sidebar runs
    /// the full window height beside it, so the toolbar starts after it).
    /// With the sidebar collapsed on macOS the traffic lights float over this
    /// row's leading edge, so it keeps the OS inset then.
    fn toolbar_row(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let bar_height = theme.skin.title_bar_height_px();
        let (fill, border) = (material::toolbar_fill(theme, cx), theme.color.border);
        let leading = if self.sidebar_collapsed {
            titlebar::content_leading_inset(window)
        } else {
            px(12.)
        };

        // Window drag / maximize / the min-max-close controls live on
        // `menu_bar_row` under CSD (Linux) — not here. They used to sit at the
        // end of this row, but that's the same row as the competition toolbar,
        // so the toolbar visibly shifted left whenever the controls appeared.
        let native = glass::active();
        // On macOS the toggle lives in the sidebar, beside the traffic lights,
        // while the sidebar is open — and takes the same spot here once it's
        // collapsed, so it never moves.
        let sidebar_owns_toggle = !self.sidebar_collapsed
            && titlebar::sidebar_top_inset(window, bar_height) > px(0.);
        div()
            .id("title-bar")
            .relative()
            .flex_none()
            .h(bar_height)
            .bg(fill)
            // The glass tier has no divider: the band's flat colour and the
            // content's scroll-edge fade meet seamlessly.
            .when(!native, |el| el.border_b_1().border_color(border))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .size_full()
                    .pl(leading)
                    .pr(px(12.))
                    .when(!sidebar_owns_toggle, |el| {
                        // The margin is *outside* the glass capsule.
                        el.child(div().flex_none().mx(px(6.)).child(
                            ToolbarGroup::new("toolbar-toggle").chromeless().button(
                                Button::icon("toggle-sidebar", Icon::PanelLeft)
                                    .oval()
                                    .tooltip(cx.t("toolbar.toggle-sidebar"))
                                    .on_click(|_, window, cx| {
                                        window.dispatch_action(Box::new(ToggleSidebar), cx)
                                    }),
                            ),
                        ))
                    })
                    .child(self.toolbar.clone()),
            )
    }

    /// The min / max / close buttons — drawn only under client-side decorations.
    fn window_controls(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let maximized = window.is_maximized();

        let button = |id: &'static str, icon: Icon, area: WindowControlArea, danger: bool| {
            div()
                .id(id)
                .window_control_area(area)
                .flex()
                .items_center()
                .justify_center()
                .size(px(28.))
                .rounded(px(6.))
                .cursor_pointer()
                .text_color(c.muted_foreground)
                .hover(|el| {
                    if danger {
                        el.bg(c.critical).text_color(c.destructive_foreground)
                    } else {
                        el.bg(gpui::Hsla {
                            a: 0.10,
                            ..c.foreground
                        })
                        .text_color(c.foreground)
                    }
                })
                .child(icon.size(px(15.)))
        };

        div()
            .flex()
            .items_center()
            .gap(px(2.))
            .child(
                button(
                    "wc-min",
                    Icon::WindowMinimize,
                    WindowControlArea::Min,
                    false,
                )
                .on_click(cx.listener(|_, _, window, _cx| window.minimize_window())),
            )
            .child(
                button(
                    "wc-max",
                    if maximized {
                        Icon::WindowRestore
                    } else {
                        Icon::WindowMaximize
                    },
                    WindowControlArea::Max,
                    false,
                )
                .on_click(cx.listener(|_, _, window, _cx| window.zoom_window())),
            )
            .child(
                button("wc-close", Icon::Close, WindowControlArea::Close, true)
                    .on_click(cx.listener(|_, _, window, _cx| window.remove_window())),
            )
    }

    /// The in-app menu-bar strip — only on skins gpui doesn't give a native bar
    /// (Windows, Linux). Sits above the toolbar band.
    ///
    /// Under client-side decorations (Linux) this row — not the toolbar band
    /// below it — also drives window drag / maximize and carries the
    /// min/max/close controls, GNOME/Adwaita-CSD-style: they sit beside the
    /// hamburger button, so the toolbar band never shifts to make room for
    /// them.
    fn menu_bar_row(&self, window: &Window, cx: &Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let owns_chrome = titlebar::owns_window_chrome(window);

        div()
            .id("menu-bar-row")
            .flex()
            .flex_none()
            .items_center()
            // Always fully opaque, unlike the rest of the chrome — the
            // in-app menu bar's own dropdowns/popovers already sit above
            // arbitrary content, so a translucent strip behind them reads as
            // a rendering bug rather than "Liquid Glass".
            .bg(theme.color.chrome)
            .border_b_1()
            .border_color(theme.color.border)
            .when(owns_chrome, |el| {
                el.window_control_area(WindowControlArea::Drag)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, _cx| this.drag_armed = true),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _window, _cx| this.drag_armed = false),
                    )
                    .on_mouse_down_out(cx.listener(|this, _, _window, _cx| this.drag_armed = false))
                    .on_mouse_move(cx.listener(|this, _, window, _cx| {
                        if this.drag_armed {
                            this.drag_armed = false;
                            window.start_window_move();
                        }
                    }))
                    .on_click(cx.listener(|_, ev: &ClickEvent, window, _cx| {
                        if ev.click_count() >= 2 {
                            window.zoom_window();
                        }
                    }))
            })
            .child(self.menu_bar.clone())
            .when(owns_chrome, |el| {
                el.child(div().flex_1())
                    .child(self.window_controls(window, cx))
            })
    }

    /// Show / hide the sidebar. Re-opening wipes the resizable state so the
    /// split re-lays-out cleanly at the remembered [`Self::sidebar_width`].
    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        if !self.sidebar_collapsed {
            self.resizable_state.update(cx, |state, _| state.clear());
        }
        cx.notify();
    }

    /// The `[sidebar | detail]` body. When the sidebar is open it's a
    /// `h_resizable` split (drag the divider, clamped to
    /// [`SIDEBAR_MIN`]…[`SIDEBAR_MAX`]); releasing the handle below
    /// [`SIDEBAR_SNAP`] snaps it to hidden. When hidden, the detail pane fills
    /// the width and a short fade plays when it comes back.
    fn body(
        &mut self,
        window: &mut Window,
        content_fill: gpui::Hsla,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let row = div().flex().flex_1().min_h(px(0.));

        // The content column: toolbar band on top, detail beneath. On the glass
        // tier the band must stay transparent so the native backing and
        // capsules beneath gpui show through — an opaque fill on the column
        // would paint over them — so the opaque content colour goes on the
        // detail area only. (Elsewhere the band paints its own fill.)
        let detail = div()
            .relative()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            // Native backing for the whole column (glass tier), on this
            // unpadded wrapper — see `GlassRole::ContentBacking`.
            .child(glass::region("content-backing", GlassRole::ContentBacking))
            .child(self.toolbar_row(window, cx))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .bg(content_fill)
                    .child(self.detail.clone()),
            );

        if self.sidebar_collapsed {
            return row.child(detail).into_any_element();
        }

        let motion = cx.theme().skin.motion(Duration::from_millis(500));
        let sidebar = div()
            .size_full()
            .child(self.sidebar.clone())
            .with_animation(
                "sidebar-fade",
                Animation::new(motion).with_easing(ease_out_quint()),
                |el, t| el.opacity(t),
            );

        let weak = cx.weak_entity();
        let split = h_resizable("main-split")
            .with_state(&self.resizable_state)
            .on_resize(move |state, _window, cx| {
                let width = state
                    .read(cx)
                    .sizes()
                    .first()
                    .copied()
                    .unwrap_or(SIDEBAR_WIDTH);
                weak.update(cx, |this, cx| {
                    if width < SIDEBAR_SNAP {
                        this.sidebar_collapsed = true;
                    } else {
                        this.sidebar_width = width;
                    }
                    cx.notify();
                })
                .ok();
            })
            .child(
                resizable_panel()
                    .size(self.sidebar_width)
                    .size_range(SIDEBAR_MIN..SIDEBAR_MAX)
                    .flex_none()
                    .child(sidebar),
            )
            .child(resizable_panel().child(detail));

        row.child(split).into_any_element()
    }
}

impl Render for AppShell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.sync_window_edited(window, cx);
        self.sync_menus(window, cx);

        // Keep the shell on the dispatch path so its menu-command handlers are
        // always reachable — otherwise, with focus cleared, macOS greys them.
        if window.is_window_active() && window.focused(cx).is_none() {
            let handle = self.focus_handle.clone();
            window.focus(&handle, cx);
        }

        let has_competition = self.store.read(cx).selected_id().is_some();
        let has_table_selection = self.detail.read(cx).has_table_selection();
        let (can_undo, can_redo) = self.store.read(cx).selected_undo_state();

        let theme = cx.theme();
        let font = theme.skin.font_family();
        let root_fill = material::root_fill(theme, cx);
        let content_fill = material::content_fill(theme);

        let shell = div()
            .track_focus(&self.focus_handle)
            .key_context("AppShell")
            .size_full()
            .flex()
            .flex_col()
            .bg(root_fill)
            .text_color(theme.color.foreground)
            .when_some(font, |el, family| el.font_family(family))
            // -- always-available commands ------------------------------------
            .on_action(cx.listener(|this, _: &NewCompetition, window, cx| {
                this.detail.update(cx, |detail, cx| {
                    detail.open_new_competition_dialog(window, cx)
                });
            }))
            .on_action(cx.listener(Self::handle_import))
            .on_action(cx.listener(Self::handle_export_all))
            .on_action(cx.listener(Self::handle_show_trash))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _cx| window.remove_window()))
            .on_action(cx.listener(|_, _: &Minimize, window, _cx| window.minimize_window()))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, cx| {
                window.toggle_fullscreen();
                cx.notify();
            }))
            .on_action(cx.listener(|this, _: &TogglePreview, _window, cx| {
                this.toggle_preview(cx);
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _window, cx| {
                this.toggle_sidebar(cx);
            }))
            .when(updater::available(), |el| {
                el.on_action(cx.listener(|this, _: &CheckForUpdates, _window, cx| {
                    this.updater.update(cx, |u, cx| u.check(true, cx));
                }))
            })
            // -- undo / redo (enabled only when a step is available) ----------
            .when(can_undo, |el| {
                el.on_action(cx.listener(|this, _: &Undo, _window, cx| {
                    this.store.update(cx, |store, cx| store.undo_selected(cx));
                }))
            })
            .when(can_redo, |el| {
                el.on_action(cx.listener(|this, _: &Redo, _window, cx| {
                    this.store.update(cx, |store, cx| store.redo_selected(cx));
                }))
            })
            // -- enabled only with a selected competition ---------------------
            .when(has_competition, |el| {
                el.on_action(cx.listener(|this, _: &AddJudgingTable, window, cx| {
                    this.detail
                        .update(cx, |detail, cx| detail.open_wizard(window, cx));
                }))
                .on_action(cx.listener(|this, _: &CompetitionSettings, window, cx| {
                    this.detail
                        .update(cx, |detail, cx| detail.open_meta_dialog(window, cx));
                }))
                .on_action(cx.listener(|this, _: &DeleteCompetitions, window, cx| {
                    this.sidebar
                        .update(cx, |sidebar, cx| sidebar.request_delete_selection(window, cx));
                }))
                .on_action(cx.listener(Self::handle_export))
                // ExportPdf / ExportDocx have no handler yet → stay greyed.
            })
            // -- enabled only with a selected judging table ------------------
            .when(has_table_selection, |el| {
                el.on_action(cx.listener(|this, _: &DuplicateJudgingTable, window, cx| {
                    this.detail
                        .update(cx, |detail, cx| detail.request_duplicate(window, cx));
                }))
                .on_action(cx.listener(
                    |this, _: &DeleteJudgingTable, window, cx| {
                        this.detail
                            .update(cx, |detail, cx| detail.request_delete(window, cx));
                    },
                ))
            })
            .when(skin_menu::in_app(cx), |el| {
                el.child(self.menu_bar_row(window, cx))
            })
            .child(self.body(window, content_fill, cx))
            // Last child: flushes this frame's glass regions to the native backdrop.
            .child(glass::end_frame())
            .children(self.updater_toast(cx));

        // Client-side decorations (Linux): wrap in the frame chrome + resize
        // gutter. A pass-through on macOS / Windows.
        decorations::apply_frame(shell, window, cx)
    }
}

/// Show a modal warning from an async context.
fn alert(cx: &mut gpui::AsyncWindowContext, message: &str) {
    warn!("alert shown to the user: {message}");
    let message = message.to_owned();
    if cx
        .update(|window, cx| {
            // Fire-and-forget: we don't care which button the user hits.
            drop(window.prompt(PromptLevel::Warning, &message, None, &["OK"], cx));
        })
        .is_err()
    {
        warn!("could not show the alert — the window is already gone");
    }
}

/// [`alert`], resolving `key` through the active [`crate::i18n::Locale`] first.
fn alert_t(cx: &mut gpui::AsyncWindowContext, key: &str) {
    let message = cx
        .update(|_, cx| cx.t(key).to_string())
        .unwrap_or_else(|_| key.to_owned());
    alert(cx, &message);
}

/// [`alert`], resolving `key` + `args` through the active [`crate::i18n::Locale`]
/// first (see [`crate::i18n::ActiveLocale::t_fmt`]).
fn alert_t_fmt(cx: &mut gpui::AsyncWindowContext, key: &str, args: &[(&str, &str)]) {
    let message = cx
        .update(|_, cx| cx.t_fmt(key, args))
        .unwrap_or_else(|_| key.to_owned());
    alert(cx, &message);
}

/// A background-executor export failure — kept as a translation key + dynamic
/// detail rather than a formatted string, since the compile/write closure
/// that produces it has no `Locale` access; [`Self::detail`] resolves it back
/// on the main thread once the async continuation has `cx` again.
#[derive(Debug)]
enum WriteError {
    Io(String),
    /// `dtb_ke_export`'s own error text (e.g. a Typst diagnostic) — already
    /// whatever language it produces; not re-translated here.
    Compile(String),
}

impl WriteError {
    fn detail(&self, cx: &mut gpui::AsyncWindowContext) -> String {
        match self {
            Self::Io(e) => cx
                .update(|_, cx| cx.t_fmt("app.export-write-failed", &[("detail", e)]))
                .unwrap_or_else(|_| e.clone()),
            Self::Compile(e) => e.clone(),
        }
    }
}

/// A sensible starting directory for a save / open dialog.
fn default_dir() -> std::path::PathBuf {
    dirs::document_dir()
        .or_else(dirs::home_dir)
        .unwrap_or_else(|| std::path::PathBuf::from("."))
}

thread_local! {
    /// The single main window, while one is open. `open_main_window` activates
    /// this instead of opening a second; `cx.on_reopen` (the macOS dock icon)
    /// re-opens it after it's been closed.
    static MAIN_WINDOW: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
}

/// Open the application's main window — or, if one is already open, just bring
/// it to the front.
pub fn open_main_window(cx: &mut App) {
    if let Some(handle) = MAIN_WINDOW.with(|m| *m.borrow()) {
        if cx.windows().contains(&handle) {
            handle
                .update(cx, |_, window, _| window.activate_window())
                .ok();
            return;
        }
        MAIN_WINDOW.with(|m| *m.borrow_mut() = None);
    }

    info!("opening the main window");
    let options = skin_window::main_window_options(cx);
    match cx.open_window(options, |window, cx| cx.new(|cx| AppShell::new(window, cx))) {
        Ok(handle) => {
            MAIN_WINDOW.with(|m| *m.borrow_mut() = Some(handle.into()));
            handle
                .update(cx, |_, window, cx| {
                    material::sync_native_backdrop(window, cx)
                })
                .ok();
        }
        Err(err) => error!("failed to open the main window: {err}"),
    }
}

/// Whether `handle` is the main window (the only one with a native backdrop).
pub fn is_main_window(handle: AnyWindowHandle) -> bool {
    MAIN_WINDOW.with(|m| *m.borrow() == Some(handle))
}
