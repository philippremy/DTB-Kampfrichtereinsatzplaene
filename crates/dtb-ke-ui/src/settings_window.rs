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

use std::cell::RefCell;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, FocusHandle, Focusable, InteractiveElement,
    IntoElement, Keystroke, ParentElement, Render, ScrollHandle, SharedString, Size,
    StatefulInteractiveElement, Styled, Subscription, TitlebarOptions, Window, WindowBounds,
    WindowKind, WindowOptions, div, point, prelude::FluentBuilder, px,
};

use crate::components::icon::Icon;
use crate::components::kbd::Kbd;
use crate::components::toggle::Toggle;
use crate::components::{Button, ButtonTone};
use crate::keymap::{self, Conflict};
use crate::settings::{AutosaveDelay, LogLevel, Settings};
use crate::theme::{ActiveTheme, ThemeMode};

const TITLE: &str = "Einstellungen";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    General,
    Keybindings,
    Advanced,
}

impl Tab {
    const ALL: [Tab; 3] = [Tab::General, Tab::Keybindings, Tab::Advanced];

    fn label(self) -> &'static str {
        match self {
            Tab::General => "Allgemein",
            Tab::Keybindings => "Tastenkürzel",
            Tab::Advanced => "Erweitert",
        }
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
    let existing = OPEN.with(|h| *h.borrow());
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
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
            title: Some(TITLE.into()),
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
        }
    }

    fn select_tab(&mut self, tab: Tab, cx: &mut Context<Self>) {
        if self.tab != tab {
            self.tab = tab;
            self.stop_recording(cx);
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
        let conflict = keymap::conflict(action, &keystroke, &self.overrides);
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
                div()
                    .id(tab.label())
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
                    .child(tab.label())
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
            .id("settings-content")
            .flex_1()
            .min_w(px(0.))
            .h_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .bg(c.background)
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
                            .child(self.tab.label()),
                    )
                    .child(body),
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
            .child(section_label("Erscheinungsbild", &c))
            .child(setting_row(
                "Farbschema",
                "Helles oder dunkles Erscheinungsbild — oder automatisch dem System folgen.",
                &c,
                self.theme_switch(mode, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                "Bewegungen reduzieren",
                "Blendet Ein-/Ausblendungen und Übergänge aus. Nützlich bei Bewegungsempfindlichkeit.",
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
                "Transparenz reduzieren",
                "Stellt jedes Fenster vollständig deckend dar — ohne Unschärfe, Mica oder Acrylic. \
                 Nützlich bei eingeschränkter Grafikleistung oder für besseren Kontrast.",
                &c,
                Toggle::new("reduce-transparency", settings.reduce_transparency)
                    .on_change(cx.processor(|_, next: bool, _window, cx| {
                        Settings::update(cx, move |s| s.reduce_transparency = next);
                        crate::material::apply_background_to_all_windows(cx);
                    }))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(section_label("Bearbeiten", &c))
            .child(setting_row(
                "Automatisch speichern",
                "Wie lange nach der letzten Änderung gewartet wird, bevor der Wettkampf gespeichert wird.",
                &c,
                self.autosave_switch(settings.autosave, cx),
            ))
            .child(divider(&c))
            .child(section_label("Aktualisierung", &c))
            .child(setting_row(
                "Automatisch nach Updates suchen",
                if crate::updater::available() {
                    "Prüft im Hintergrund auf neue Versionen und zeigt eine Benachrichtigung an. \
                     Die Aktualisierung wird nie ohne Bestätigung installiert."
                } else {
                    "In dieser Programmversion ist die Aktualisierungsfunktion nicht verfügbar \
                     (kein Prüfschlüssel eingebettet). Die Einstellung wird trotzdem gespeichert."
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
                "Aktualisierungskanal",
                settings.update_channel.description(),
                &c,
                self.channel_switch(settings.update_channel, cx),
            ))
            .when_some(settings.skipped_update.clone(), |el, version| {
                el.child(divider(&c)).child(setting_row(
                    "Übersprungene Version",
                    &format!(
                        "Version {version} wurde übersprungen und wird bei automatischen \
                         Prüfungen ignoriert, bis eine neuere Version erscheint."
                    ),
                    &c,
                    Button::new("reset-skipped-update", "Zurücksetzen")
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
        let option = |mode: ThemeMode, icon: Icon| {
            let selected = mode == current;
            div()
                .id(mode.label())
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
                .child(mode.label())
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
            .child(option(ThemeMode::System, Icon::Monitor))
            .child(option(ThemeMode::Light, Icon::Sun))
            .child(option(ThemeMode::Dark, Icon::Moon))
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
        let option = |channel: UpdateChannel| {
            let selected = channel == current;
            div()
                .id(channel.label())
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
                .child(channel.label())
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
            .child(option(UpdateChannel::Stable))
            .child(option(UpdateChannel::Tip))
            .into_any_element()
    }

    fn autosave_switch(&self, current: AutosaveDelay, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let short = |d: AutosaveDelay| match d {
            AutosaveDelay::Immediate => "Sofort",
            AutosaveDelay::Half => "0,5 s",
            AutosaveDelay::One => "1 s",
            AutosaveDelay::Two => "2 s",
            AutosaveDelay::Five => "5 s",
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
                div()
                    .id(short(delay))
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
                    .child(short(delay))
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
        let auto_hint = format!(
            "„Automatisch“ = derzeit {}. Eine feste Stufe überschreibt die Erkennung sofort, \
             auch im laufenden Betrieb.",
            filter_label(dtb_ke_log::effective_level())
        );

        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .child(section_label("Protokollierung", &c))
            .child(setting_row(
                "Protokollstufe",
                &auto_hint,
                &c,
                self.loglevel_switch(settings.log_level, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                "Protokolle ansehen",
                "Öffnet das Protokollfenster mit allen Sitzungsdateien.",
                &c,
                Button::new("open-logs", "Protokolle öffnen")
                    .small()
                    .on_click(|_, _window, cx| crate::logs_window::open(cx))
                    .into_any_element(),
            ))
    }

    fn loglevel_switch(&self, current: LogLevel, cx: &mut Context<Self>) -> gpui::AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
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
                div()
                    .id(level.label())
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
                    .child(level.label())
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

        let mut rows: Vec<gpui::AnyElement> = Vec::new();
        for (action, label) in keymap::configurable() {
            rows.push(self.binding_row(action, label, cx));
        }

        let fixed: Vec<(&'static str, String)> = keymap::defaults()
            .into_iter()
            .filter(|row| !row.configurable && row.default.resolve().is_some())
            .map(|row| {
                let name = row.name();
                (
                    name,
                    crate::menu::label_for(name).unwrap_or_else(|| name.to_string()),
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
                    .child(
                        "Klicke „Aufnehmen“ und drücke die gewünschte Tastenkombination. \
                         Überschneidungen mit anderen Befehlen oder Systemkürzeln werden gemeldet.",
                    ),
            )
            .child(section_label("Anpassbar", &c))
            .children(rows)
            .child(divider(&c))
            .child(section_label("Fest vergeben", &c))
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
                    .child("Taste drücken … (Esc bricht ab)"),
            );
            right = right.child(
                Button::new("cancel-rec", "Abbrechen")
                    .small()
                    .on_click(cx.listener(|this, _, _window, cx| this.stop_recording(cx))),
            );
        } else if let Some(pending) = pending {
            right = right
                .child(Kbd::new(pending.keystroke.clone()))
                .child(
                    Button::new("apply-rec", "Übernehmen")
                        .small()
                        .tone(if pending.conflict.is_some() {
                            ButtonTone::Danger
                        } else {
                            ButtonTone::Primary
                        })
                        .on_click(cx.listener(|this, _, _window, cx| this.apply_pending(cx))),
                )
                .child(
                    Button::new("discard-rec", "Verwerfen")
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
                    Button::new(SharedString::from(format!("rec-{action}")), "Aufnehmen")
                        .small()
                        .on_click(cx.listener(move |this, _, _window, cx| {
                            this.start_recording(action, cx)
                        })),
                );
            if overridden {
                right = right.child(
                    Button::new(
                        SharedString::from(format!("reset-{action}")),
                        "Zurücksetzen",
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
                .child(conflict.message())
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
                    .child(TITLE),
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

fn section_label(text: &str, c: &crate::theme::PaletteColors) -> impl IntoElement {
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

/// German label for a resolved `LevelFilter` (for the "currently …" hint).
fn filter_label(filter: log::LevelFilter) -> &'static str {
    match filter {
        log::LevelFilter::Off => "Aus",
        log::LevelFilter::Error => "Fehler",
        log::LevelFilter::Warn => "Warnung",
        log::LevelFilter::Info => "Info",
        log::LevelFilter::Debug => "Debug",
        log::LevelFilter::Trace => "Trace",
    }
}

fn setting_row(
    title: &str,
    description: &str,
    c: &crate::theme::PaletteColors,
    control: gpui::AnyElement,
) -> impl IntoElement {
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
