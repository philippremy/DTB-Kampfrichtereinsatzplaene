//! The settings window.
//!
//! A single window, Zed-style: a left navigation rail (Allgemein · Tastenkürzel)
//! and a scrolling content pane. At most one instance is open; re-triggering
//! `app::OpenSettings` just activates it.
//!
//! * **Allgemein** — appearance (a three-way System / Hell / Dunkel switch),
//!   reduce-motion, reduce-transparency (forces every window fully opaque —
//!   see [`crate::material`] — applied live to already-open windows via
//!   [`crate::material::apply_background_to_all_windows`]), autosave delay.
//! * **Tastenkürzel** — every configurable menu command with its shortcut shown
//!   as a single-block [`Kbd`]. "Aufnehmen" records a new chord via
//!   [`App::intercept_keystrokes`]; the capture is checked against the other
//!   commands and a list of reserved OS shortcuts before it can be applied.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, Keystroke, MouseButton, ParentElement, Pixels, Render, ScrollHandle, SharedString,
    Size, StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowKind, WindowOptions, anchored, canvas, deferred, div, point, prelude::FluentBuilder, px,
};
use gpui_base::Scrollbar;

use crate::components::icon::Icon;
use crate::components::kbd::Kbd;
use crate::components::toggle::Toggle;
use crate::components::{Button, ButtonTone};
use crate::i18n::ActiveLocale;
use crate::keymap::{self, Conflict};
use crate::settings::{AutosaveDelay, LogLevel, Settings};
use crate::theme::{ActiveTheme, ThemeMode};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tab {
    General,
    Keybindings,
    Advanced,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::General, Tab::Keybindings, Tab::Advanced];

    fn label(self, locale: &crate::i18n::Locale) -> SharedString {
        let key = match self {
            Tab::General => "settings.tab-general",
            Tab::Keybindings => "settings.tab-keybindings",
            Tab::Advanced => "settings.tab-advanced",
        };
        locale.t(key)
    }

    fn icon(self) -> Icon {
        match self {
            Tab::General => Icon::Settings,
            Tab::Keybindings => Icon::Menu,
            Tab::Advanced => Icon::Warning,
        }
    }
}

thread_local! {
    static OPEN: RefCell<Option<AnyWindowHandle>> = const { RefCell::new(None) };
}

/// Open (or focus) the settings window.
pub fn open(cx: &mut App) {
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
    match cx.open_window(options, |_, cx| cx.new(SettingsWindow::new)) {
        Ok(handle) => OPEN.with(|h| *h.borrow_mut() = Some(handle.into())),
        Err(err) => log::error!("Einstellungsfenster konnte nicht geöffnet werden: {err}"),
    }
}

fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(780.), px(560.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(cx.t("settings.window-title")),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_minimizable: false,
        window_min_size: Some(Size::new(px(780.), px(560.))),
        ..Default::default()
    }
}

/// A captured-but-not-yet-applied re-binding.
struct Pending {
    action: &'static str,
    keystroke: String,
    conflict: Option<Conflict>,
}

pub struct SettingsWindow {
    tab: Tab,
    focus: FocusHandle,
    scroll: ScrollHandle,

    /// Working copy of the key-binding overrides, seeded from and written back to
    /// [`Settings`].
    overrides: keymap::Overrides,
    /// The command currently listening for a chord.
    recording: Option<&'static str>,
    /// The last captured chord, awaiting apply / discard.
    pending: Option<Pending>,
    /// The keystroke interceptor, live only while recording.
    intercept: Option<Subscription>,

    /// Whether the language dropdown's popover is open.
    language_open: bool,
    /// Absolute bounds of the language dropdown trigger, captured during
    /// prepaint (via a `canvas` probe — not an entity update, which would
    /// dead-lock) so the popover can anchor to it. Same convention as
    /// `detail::meta_dialog::MetaDialog`'s organisation selector.
    language_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
}

impl Drop for SettingsWindow {
    fn drop(&mut self) {
        OPEN.with(|h| *h.borrow_mut() = None);
    }
}

impl SettingsWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        Self {
            tab: Tab::General,
            focus: cx.focus_handle(),
            scroll: ScrollHandle::new(),
            overrides: Settings::global(cx).keybindings,
            recording: None,
            pending: None,
            intercept: None,
            language_open: false,
            language_bounds: Rc::new(Cell::new(None)),
        }
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.stop_recording(cx);
            self.language_open = false;
            self.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    // ── key-binding recording ────────────────────────────────────────────

    fn start_recording(&mut self, action: &'static str, cx: &mut Context<Self>) {
        self.pending = None;
        self.recording = Some(action);

        // A *weak* handle — the interceptor lives inside `self.intercept`, so a
        // strong one would be a reference cycle (window never dropped).
        let entity = cx.weak_entity();
        self.intercept = Some(cx.intercept_keystrokes(move |event, _window, cx| {
            let keystroke = event.keystroke.clone();
            entity
                .update(cx, |this, cx| this.on_recorded(&keystroke, cx))
                .ok();
            cx.stop_propagation();
        }));
        cx.notify();
    }

    fn stop_recording(&mut self, cx: &mut Context<Self>) {
        self.recording = None;
        self.pending = None;
        self.intercept = None;
        cx.notify();
    }

    fn on_recorded(&mut self, ks: &Keystroke, cx: &mut Context<Self>) {
        let Some(action) = self.recording else {
            return;
        };

        // A lone modifier press — keep listening for the full chord.
        if matches!(
            ks.key.as_str(),
            "cmd"
                | "ctrl"
                | "control"
                | "alt"
                | "shift"
                | "platform"
                | "function"
                | "fn"
                | "super"
                | "win"
        ) {
            return;
        }

        // Escape (alone) cancels.
        if ks.key == "escape" && !ks.modifiers.modified() {
            self.stop_recording(cx);
            return;
        }

        let keystroke = ks.unparse();
        let conflict = keymap::conflict(
            action,
            &keystroke,
            &self.overrides,
            cx.global::<crate::i18n::Locale>(),
        );
        self.recording = None;
        self.intercept = None;
        self.pending = Some(Pending {
            action,
            keystroke,
            conflict,
        });
        cx.notify();
    }

    fn apply_pending(&mut self, cx: &mut Context<Self>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let previous = self.overrides.clone();
        self.overrides
            .insert(pending.action.to_string(), pending.keystroke.clone());
        keymap::rebind(pending.action, Some(&pending.keystroke), &previous, cx);
        let action = pending.action.to_string();
        let keystroke = pending.keystroke.clone();
        log::info!("keybinding override: {action} → {keystroke}");
        Settings::update(cx, move |settings| {
            settings.keybindings.insert(action, keystroke);
        });
        cx.notify();
    }

    fn reset_binding(&mut self, action: &'static str, cx: &mut Context<Self>) {
        if self.overrides.remove(action).is_none() {
            return;
        }
        log::info!("keybinding override cleared: {action} → default");
        let previous = {
            let mut map = self.overrides.clone();
            map.insert(action.to_string(), "".to_string());
            map
        };
        // Unbind whatever the override installed, then restore the default.
        keymap::rebind(
            action,
            keymap::default_keystroke(action).as_deref(),
            &previous,
            cx,
        );
        let owned = action.to_string();
        Settings::update(cx, move |settings| {
            settings.keybindings.remove(&owned);
        });
        cx.notify();
    }

    // ── content ─────────────────────────────────────────────────────────

    fn nav_rail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        div()
            .flex()
            .flex_none()
            .flex_col()
            .w(px(196.))
            .h_full()
            .border_r_1()
            .border_color(c.border)
            .bg(c.chrome)
            .pt(px(12.))
            .px(px(10.))
            .gap(px(2.))
            .children(Tab::ALL.into_iter().map(|tab| {
                let selected = self.tab == tab;
                let label = tab.label(cx.global::<crate::i18n::Locale>());
                div()
                    .id(SharedString::from(format!("tab-{tab:?}")))
                    .flex()
                    .items_center()
                    .gap(px(9.))
                    .h(px(32.))
                    .px(px(10.))
                    .rounded(cx.theme().skin.radius_control_px())
                    .text_size(px(13.5))
                    .when(selected, |el| {
                        el.bg(gpui::Hsla {
                            a: 0.14,
                            ..c.primary
                        })
                        .text_color(c.primary)
                    })
                    .when(!selected, |el| {
                        el.text_color(c.muted_foreground)
                            .cursor_pointer()
                            .hover(|el| el.text_color(c.foreground))
                    })
                    .child(tab.icon().size(px(15.)).color(if selected {
                        c.primary
                    } else {
                        c.muted_foreground
                    }))
                    .child(label)
                    .on_click(cx.listener(move |this, _, _, cx| this.select_tab(tab, cx)))
            }))
    }

    fn content(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let body = match self.tab {
            Tab::General => self.general_tab(cx).into_any_element(),
            Tab::Keybindings => self.keybindings_tab(cx).into_any_element(),
            Tab::Advanced => self.advanced_tab(cx).into_any_element(),
        };

        div()
            .relative()
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .bg(c.background)
            .child(Scrollbar::vertical(&self.scroll))
            .child(
                div()
                    .id("settings-content")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .w_full()
                            .max_w(px(620.))
                            .px(px(32.))
                            .py(px(26.))
                            .gap(px(4.))
                            .child(
                                div()
                                    .text_size(px(19.))
                                    .font_weight(gpui::FontWeight::SEMIBOLD)
                                    .child(self.tab.label(cx.global::<crate::i18n::Locale>())),
                            )
                            .child(body),
                    ),
            )
    }

    fn general_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let mode = crate::theme::Theme::mode(cx);
        let settings = Settings::global(cx);

        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .child(section_label(cx.t("settings.general.language-section"), &c))
            .child(setting_row(
                cx.t("settings.general.language-title"),
                cx.t("settings.general.language-description"),
                &c,
                self.language_switch(settings.locale.clone(), cx),
            ))
            .child(divider(&c))
            .child(section_label(
                cx.t("settings.general.appearance-section"),
                &c,
            ))
            .child(setting_row(
                cx.t("settings.general.theme-title"),
                cx.t("settings.general.theme-description"),
                &c,
                self.theme_switch(mode, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t("settings.general.accent-title"),
                if cfg!(any(target_os = "macos", target_os = "windows")) {
                    cx.t("settings.general.accent-description")
                } else {
                    cx.t("settings.general.accent-description-unavailable")
                },
                &c,
                Toggle::new("use-system-accent", settings.use_system_accent_color)
                    .disabled(!cfg!(any(target_os = "macos", target_os = "windows")))
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        Settings::update(cx, move |s| s.use_system_accent_color = next);
                        crate::theme::Theme::reload(cx);
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t("settings.general.reduce-motion-title"),
                cx.t("settings.general.reduce-motion-description"),
                &c,
                Toggle::new("reduce-motion", settings.reduce_motion)
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        cx.set_reduce_motion(next);
                        Settings::update(cx, move |s| s.reduce_motion = next);
                        cx.refresh_windows();
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t("settings.general.reduce-transparency-title"),
                cx.t("settings.general.reduce-transparency-description"),
                &c,
                Toggle::new("reduce-transparency", settings.reduce_transparency)
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        Settings::update(cx, move |s| s.reduce_transparency = next);
                        crate::material::apply_background_to_all_windows(cx);
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(section_label(cx.t("settings.general.editing-section"), &c))
            .child(setting_row(
                cx.t("settings.general.autosave-title"),
                cx.t("settings.general.autosave-description"),
                &c,
                self.autosave_switch(settings.autosave, cx),
            ))
            .child(divider(&c))
            .child(section_label(cx.t("settings.general.backup-section"), &c))
            .child(setting_row(
                cx.t("settings.general.backup-title"),
                cx.t("settings.general.backup-description"),
                &c,
                Toggle::new("auto-backup", settings.auto_backup)
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        Settings::update(cx, move |s| s.auto_backup = next);
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(section_label(cx.t("settings.general.update-section"), &c))
            .child(setting_row(
                cx.t("settings.general.auto-update-title"),
                if crate::updater::available() {
                    cx.t("settings.general.auto-update-description")
                } else {
                    cx.t("settings.general.auto-update-description-unavailable")
                },
                &c,
                Toggle::new("auto-update", settings.auto_update)
                    .disabled(!crate::updater::available())
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        Settings::update(cx, move |s| s.auto_update = next);
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t("settings.general.update-channel-title"),
                settings
                    .update_channel
                    .description(cx.global::<crate::i18n::Locale>()),
                &c,
                self.channel_switch(settings.update_channel, cx),
            ))
            .when_some(settings.skipped_update.clone(), |el, version| {
                let description = cx.t_fmt(
                    "settings.general.skipped-update-description",
                    &[("version", &version)],
                );
                el.child(divider(&c)).child(setting_row(
                    cx.t("settings.general.skipped-update-title"),
                    description,
                    &c,
                    Button::new(
                        "reset-skipped-update",
                        cx.t("settings.general.skipped-update-reset-button"),
                    )
                    .small()
                    .on_click(|_, _window, cx| {
                        log::info!("update-skip reset from settings");
                        Settings::update(cx, |s| s.skipped_update = None);
                    })
                    .into_any_element(),
                ))
            })
    }

    fn theme_switch(&self, current: ThemeMode, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let locale = cx.global::<crate::i18n::Locale>().clone();
        let option = |mode: ThemeMode, icon: Icon, cx: &mut Context<Self>| {
            let selected = mode == current;
            let label = mode.label(&locale);
            div()
                .id(SharedString::from(format!("theme-{mode:?}")))
                .flex()
                .items_center()
                .gap(px(6.))
                .h(px(28.))
                .px(px(12.))
                .rounded(px((cx.theme().skin.radius_control - 2.0).max(2.0)))
                .text_size(px(12.5))
                .when(selected, |el| {
                    el.bg(c.surface).shadow_xs().text_color(c.foreground)
                })
                .when(!selected, |el| {
                    el.text_color(c.muted_foreground)
                        .cursor_pointer()
                        .hover(|el| el.text_color(c.foreground))
                })
                .child(icon.size(px(14.)).color(if selected {
                    c.primary
                } else {
                    c.muted_foreground
                }))
                .child(label)
                .on_click(cx.listener(move |_, _, _window, cx| {
                    crate::theme::Theme::set_mode(mode, cx);
                }))
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.chrome)
            .child(option(ThemeMode::System, Icon::Monitor, cx))
            .child(option(ThemeMode::Light, Icon::Sun, cx))
            .child(option(ThemeMode::Dark, Icon::Moon, cx))
            .into_any_element()
    }

    fn channel_switch(
        &self,
        current: crate::settings::UpdateChannel,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        use crate::settings::UpdateChannel;
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let available = crate::updater::available();
        let locale = cx.global::<crate::i18n::Locale>().clone();
        let option = |channel: UpdateChannel, cx: &mut Context<Self>| {
            let selected = channel == current;
            div()
                .id(SharedString::from(format!("channel-{channel:?}")))
                .flex()
                .items_center()
                .justify_center()
                .h(px(26.))
                .px(px(10.))
                .rounded(px((cx.theme().skin.radius_control - 2.0).max(2.0)))
                .text_size(px(12.))
                .when(selected, |el| {
                    el.bg(c.surface).shadow_xs().text_color(c.foreground)
                })
                .when(!selected, |el| {
                    el.text_color(c.muted_foreground).when(available, |el| {
                        el.cursor_pointer().hover(|el| el.text_color(c.foreground))
                    })
                })
                .child(channel.label(&locale))
                .when(available, |el| {
                    el.on_click(cx.listener(move |_, _, _window, cx| {
                        // A skip recorded on one channel is meaningless on the
                        // other's unrelated version sequence.
                        Settings::update(cx, move |s| {
                            s.update_channel = channel;
                            s.skipped_update = None;
                        });
                        log::info!("update channel → {channel:?}");
                    }))
                })
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.chrome)
            .when(!available, |el| el.opacity(0.5))
            .child(option(UpdateChannel::Stable, cx))
            .child(option(UpdateChannel::Tip, cx))
            .into_any_element()
    }

    /// A dropdown (not a pill row — with more than a couple of locales that
    /// would grow unbounded) built from [`crate::i18n::available_locales`]
    /// plus a leading "System" option; adding a shipped catalog needs no
    /// change here. Same popover convention as
    /// `detail::meta_dialog::MetaDialog::org_selector`.
    fn language_switch(&self, current: Option<String>, cx: &mut Context<Self>) -> gpui::AnyElement {
        use crate::i18n::Locale;
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.language_open;

        let active_name = cx.global::<Locale>().name.clone();
        let system_label = cx.t_fmt(
            "settings.general.language-system",
            &[("name", active_name.as_str())],
        );
        let current_label = match &current {
            None => system_label.clone(),
            Some(tag) => crate::i18n::available_locales()
                .into_iter()
                .find(|l| l.tag == tag.as_str())
                .map(|l| l.name)
                .unwrap_or_else(|| tag.clone()),
        };

        let list = open.then(|| self.language_bounds.get()).flatten().map(|b| {
            let anchor = point(b.origin.x, b.origin.y + b.size.height + px(3.));

            let option = |value: Option<String>, label: SharedString, cx: &mut Context<Self>| {
                let selected = value == current;
                let id = value.clone().unwrap_or_else(|| "system".to_owned());
                div()
                    .id(SharedString::from(id))
                    .px(px(8.))
                    .py(px(5.))
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .when(selected, |el| el.text_color(c.primary))
                    .hover(|el| el.bg(c.accent_soft))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _, _w, cx| {
                            this.language_open = false;
                            let next = value.clone();
                            log::info!("locale set to {next:?}");
                            Settings::update(cx, move |s| s.locale = next.clone());
                            Locale::reload(cx);
                            cx.notify();
                        }),
                    )
                    .child(label)
            };

            let mut col = div()
                .id("language-list")
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
                .occlude()
                .child(option(None, SharedString::from(system_label.clone()), cx));

            for locale in crate::i18n::available_locales() {
                col = col.child(option(
                    Some(locale.tag.to_owned()),
                    SharedString::from(locale.name),
                    cx,
                ));
            }

            // Same priority convention as `MetaDialog::org_selector` — above
            // a `gpui_base::Dialog` (this window has none, but keeps every
            // popover in the app consistent).
            deferred(anchored().position(anchor).snap_to_window().child(col))
                .with_priority(gpui_base::POPUP_PRIORITY)
        });

        let capture = self.language_bounds.clone();

        div()
            .id("language-select")
            .relative()
            .flex()
            .flex_col()
            .w(px(220.))
            .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                if this.language_open {
                    this.language_open = false;
                    cx.notify();
                }
            }))
            // Capture the *outer* bounds of the control (no padding/border of
            // its own) so the popover aligns with the trigger's visible edge.
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
                    .id("language-select-trigger")
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(6.))
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(radius)
                    .border_1()
                    .border_color(if open { c.primary } else { c.border })
                    .bg(c.surface)
                    .text_size(px(12.5))
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _w, cx| {
                        this.language_open = !this.language_open;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .flex_1()
                            .truncate()
                            .child(SharedString::from(current_label)),
                    )
                    .child(Icon::ChevronDown.size(px(13.)).color(c.muted_foreground)),
            )
            .children(list)
            .into_any_element()
    }

    fn autosave_switch(&self, current: AutosaveDelay, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let short = |d: AutosaveDelay, cx: &Context<Self>| {
            let key = match d {
                AutosaveDelay::Immediate => "settings.general.autosave-short-immediate",
                AutosaveDelay::Half => "settings.general.autosave-short-half",
                AutosaveDelay::One => "settings.general.autosave-short-one",
                AutosaveDelay::Two => "settings.general.autosave-short-two",
                AutosaveDelay::Five => "settings.general.autosave-short-five",
            };
            cx.t(key)
        };
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.chrome)
            .children(AutosaveDelay::ALL.into_iter().map(|delay| {
                let selected = delay == current;
                let label = short(delay, cx);
                div()
                    .id(SharedString::from(format!("autosave-{delay:?}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(26.))
                    .px(px(9.))
                    .rounded(px((cx.theme().skin.radius_control - 2.0).max(2.0)))
                    .text_size(px(12.))
                    .when(selected, |el| {
                        el.bg(c.surface).shadow_xs().text_color(c.foreground)
                    })
                    .when(!selected, |el| {
                        el.text_color(c.muted_foreground)
                            .cursor_pointer()
                            .hover(|el| el.text_color(c.foreground))
                    })
                    .child(label)
                    .on_click(cx.listener(move |_, _, _window, cx| {
                        log::info!("autosave delay changed to {delay:?}");
                        Settings::update(cx, move |s| s.autosave = delay);
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    fn advanced_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let settings = Settings::global(cx);
        let current = filter_label(
            dtb_ke_log::effective_level(),
            cx.global::<crate::i18n::Locale>(),
        );
        let auto_hint = cx.t_fmt(
            "settings.advanced.log-level-hint",
            &[("current", current.as_ref())],
        );

        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .child(section_label(cx.t("settings.advanced.logging-section"), &c))
            .child(setting_row(
                cx.t("settings.advanced.log-level-title"),
                auto_hint,
                &c,
                self.loglevel_switch(settings.log_level, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t("settings.advanced.view-logs-title"),
                cx.t("settings.advanced.view-logs-description"),
                &c,
                Button::new("open-logs", cx.t("settings.advanced.open-logs-button"))
                    .small()
                    .on_click(|_, _window, cx| crate::logs_window::open(cx))
                    .into_any_element(),
            ))
    }

    fn loglevel_switch(&self, current: LogLevel, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let locale = cx.global::<crate::i18n::Locale>().clone();
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(2.))
            .p(px(2.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.chrome)
            .children(LogLevel::ALL.into_iter().map(|level| {
                let selected = level == current;
                let label = level.label(&locale);
                div()
                    .id(SharedString::from(format!("loglevel-{level:?}")))
                    .flex()
                    .items_center()
                    .justify_center()
                    .h(px(26.))
                    .px(px(9.))
                    .rounded(px((cx.theme().skin.radius_control - 2.0).max(2.0)))
                    .text_size(px(12.))
                    .when(selected, |el| {
                        el.bg(c.surface).shadow_xs().text_color(c.foreground)
                    })
                    .when(!selected, |el| {
                        el.text_color(c.muted_foreground)
                            .cursor_pointer()
                            .hover(|el| el.text_color(c.foreground))
                    })
                    .child(label)
                    .on_click(cx.listener(move |_, _, _window, cx| {
                        log::info!("log level set to {level:?}");
                        Settings::update(cx, move |s| s.log_level = level);
                        dtb_ke_log::set_level(level.to_filter());
                        cx.notify();
                    }))
            }))
            .into_any_element()
    }

    fn keybindings_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;

        let locale = cx.global::<crate::i18n::Locale>().clone();
        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (action, label) in keymap::configurable(&locale) {
            rows.push(self.binding_row(action, label.to_string(), cx));
        }

        let fixed: Vec<(&'static str, String)> = keymap::defaults()
            .into_iter()
            .filter(|row| !row.configurable && row.default.resolve().is_some())
            .map(|row| {
                let name = row.name();
                (
                    name,
                    crate::menu::label_for(name, &locale)
                        .unwrap_or_else(|| SharedString::from(name))
                        .to_string(),
                )
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .child(
                div()
                    .text_size(px(12.5))
                    .text_color(c.muted_foreground)
                    .mb(px(14.))
                    .child(cx.t("settings.keybindings.hint")),
            )
            .child(section_label(
                cx.t("settings.keybindings.configurable-section"),
                &c,
            ))
            .children(rows)
            .child(divider(&c))
            .child(section_label(
                cx.t("settings.keybindings.fixed-section"),
                &c,
            ))
            .children(fixed.into_iter().map(|(action, label)| {
                let keystroke = keymap::effective_keystroke(action, &self.overrides);
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .py(px(9.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .text_color(c.muted_foreground)
                            .child(label),
                    )
                    .child(match keystroke {
                        Some(ks) => Kbd::new(ks).muted(true).into_any_element(),
                        None => Kbd::unbound().into_any_element(),
                    })
            }))
    }

    fn binding_row(
        &self,
        action: &'static str,
        label: String,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let c = cx.theme().color;
        let recording = self.recording == Some(action);
        let pending = self.pending.as_ref().filter(|p| p.action == action);
        let overridden = self.overrides.contains_key(action);
        let effective = keymap::effective_keystroke(action, &self.overrides);

        let mut right = div().flex().items_center().gap(px(8.));

        if recording {
            right = right.child(
                div()
                    .flex()
                    .items_center()
                    .h(px(28.))
                    .px(px(10.))
                    .rounded(cx.theme().skin.radius_control_px())
                    .border_1()
                    .border_dashed()
                    .border_color(c.primary)
                    .text_size(px(12.))
                    .text_color(c.primary)
                    .child(cx.t("settings.keybindings.recording-hint")),
            );
            right = right.child(
                Button::new("cancel-rec", cx.t("settings.keybindings.cancel-button"))
                    .small()
                    .on_click(cx.listener(|this, _, _window, cx| this.stop_recording(cx))),
            );
        } else if let Some(pending) = pending {
            right = right
                .child(Kbd::new(pending.keystroke.clone()))
                .child(
                    Button::new("apply-rec", cx.t("settings.keybindings.apply-button"))
                        .small()
                        .tone(if pending.conflict.is_some() {
                            ButtonTone::Danger
                        } else {
                            ButtonTone::Primary
                        })
                        .on_click(cx.listener(|this, _, _window, cx| this.apply_pending(cx))),
                )
                .child(
                    Button::new("discard-rec", cx.t("settings.keybindings.discard-button"))
                        .small()
                        .on_click(cx.listener(|this, _, _window, cx| this.stop_recording(cx))),
                );
        } else {
            right = right
                .child(match &effective {
                    Some(ks) => Kbd::new(ks.clone()).into_any_element(),
                    None => Kbd::unbound().into_any_element(),
                })
                .child(
                    Button::new(
                        SharedString::from(format!("rec-{action}")),
                        cx.t("settings.keybindings.record-button"),
                    )
                    .small()
                    .on_click(
                        cx.listener(move |this, _, _window, cx| this.start_recording(action, cx)),
                    ),
                );
            if overridden {
                right = right.child(
                    Button::new(
                        SharedString::from(format!("reset-{action}")),
                        cx.t("settings.keybindings.reset-button"),
                    )
                    .small()
                    .tone(ButtonTone::Ghost)
                    .on_click(
                        cx.listener(move |this, _, _window, cx| this.reset_binding(action, cx)),
                    ),
                );
            }
        }

        let conflict_note = pending.and_then(|p| p.conflict.as_ref()).map(|conflict| {
            div()
                .mt(px(4.))
                .flex()
                .items_center()
                .gap(px(6.))
                .text_size(px(11.5))
                .text_color(c.warn)
                .child(Icon::Warning.size(px(13.)).color(c.warn))
                .child(conflict.message(cx.global::<crate::i18n::Locale>()))
        });

        div()
            .flex()
            .flex_col()
            .py(px(8.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .min_h(px(30.))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_size(px(13.))
                            .text_color(c.foreground)
                            .child(label),
                    )
                    .child(right),
            )
            .when_some(conflict_note, |el, note| el.child(note))
            .into_any_element()
    }
}

impl Render for SettingsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let font = cx.theme().skin.font_family();
        let lead = crate::skin::titlebar::content_leading_inset(window);

        // Keep the root focused so `intercept_keystrokes` sees key events.
        if window.is_window_active() && window.focused(cx).is_none() {
            let handle = self.focus.clone();
            window.focus(&handle, cx);
        }

        div()
            .track_focus(&self.focus)
            .key_context("SettingsWindow")
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .when_some(font, |el, family| el.font_family(family))
            .child(
                // Title strip — mirrors the other secondary windows.
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
                    .child(cx.t("settings.window-title")),
            )
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.nav_rail(cx))
                    .child(self.content(cx)),
            )
    }
}

impl Focusable for SettingsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ── small building blocks ───────────────────────────────────────────────

fn section_label(
    text: impl Into<SharedString>,
    c: &crate::theme::PaletteColors,
) -> impl IntoElement {
    let text = text.into();
    div()
        .mt(px(18.))
        .mb(px(2.))
        .text_size(px(11.))
        .font_weight(gpui::FontWeight::SEMIBOLD)
        .text_color(c.muted_foreground)
        .child(text.to_uppercase())
}

fn divider(c: &crate::theme::PaletteColors) -> impl IntoElement {
    div().h(px(1.)).bg(c.border).my(px(4.))
}

/// A translated label for a resolved `LevelFilter` (for the "currently …" hint).
fn filter_label(filter: log::LevelFilter, locale: &crate::i18n::Locale) -> SharedString {
    let key = match filter {
        log::LevelFilter::Off => "settings.advanced.log-level-off",
        log::LevelFilter::Error => "settings.advanced.log-level-error",
        log::LevelFilter::Warn => "settings.advanced.log-level-warn",
        log::LevelFilter::Info => "settings.advanced.log-level-info",
        log::LevelFilter::Debug => "settings.advanced.log-level-debug",
        log::LevelFilter::Trace => "settings.advanced.log-level-trace",
    };
    locale.t(key)
}

fn setting_row(
    title: impl Into<SharedString>,
    description: impl Into<SharedString>,
    c: &crate::theme::PaletteColors,
    control: gpui::AnyElement,
) -> impl IntoElement {
    let title = title.into();
    let description = description.into();
    div()
        .flex()
        .items_start()
        .justify_between()
        .gap(px(20.))
        .py(px(12.))
        .child(
            div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .flex_1()
                .min_w(px(0.))
                .child(
                    div()
                        .text_size(px(13.5))
                        .text_color(c.foreground)
                        .child(title.to_string()),
                )
                .child(
                    div()
                        .text_size(px(12.))
                        .text_color(c.muted_foreground)
                        .child(description.to_string()),
                ),
        )
        .child(div().flex_none().pt(px(1.)).child(control))
}
