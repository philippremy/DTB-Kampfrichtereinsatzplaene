//! The hidden "Debugging" tab (see [`crate::debug`]). Every label comes from the catalogs under
//! `settings.debug.*` (`locales/*.toml`), like the rest of the settings window.

use std::rc::Rc;
use std::time::Duration;

use gpui_kit::base::input::InputState;
use gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, InteractiveElement, IntoElement, ParentElement, PromptLevel,
    SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use super::{SettingsWindow, divider, section_label, setting_row};
use crate::components::field::Field;
use crate::i18n::ActiveLocale;
use crate::components::toggle::Toggle;
use crate::components::{Button, ButtonTone};
use crate::debug::crash::{self, Kind, Thread};
use crate::debug::{DebugSettings, MailSim, SkinOverride, Tri};
use crate::settings::Settings;
use crate::theme::ActiveTheme;

/// Seconds between confirming a simulated crash and the fault, to leave time to switch windows.
const CRASH_DELAY: Duration = Duration::from_secs(3);

/// Build the update-manifest input, seeded from the stored option.
pub(super) fn manifest_input(window: &mut Window, cx: &mut Context<SettingsWindow>) -> Entity<InputState> {
    let current = Settings::global(cx).debug.update_manifest;
    cx.new(|cx| {
        let mut state = InputState::new(window, cx).placeholder("https://…/manifest.json");
        state.set_value(current, window, cx);
        state
    })
}

impl SettingsWindow {
    pub(super) fn debug_tab(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let d = Settings::global(cx).debug;
        let locale = cx.global::<crate::i18n::Locale>().clone();

        let crash_rows: Vec<AnyElement> = Kind::ALL
            .into_iter()
            .map(|kind| {
                setting_row(
                    kind.label(&locale),
                    kind.description(&locale),
                    &c,
                    Button::new(SharedString::from(format!("crash-{kind:?}")), cx.t(&key("trigger-button")))
                        .small()
                        .tone(ButtonTone::Danger)
                        .on_click(cx.listener(move |this, _, window, cx| this.confirm_crash(kind, window, cx)))
                        .into_any_element(),
                )
                .into_any_element()
            })
            .collect();

        div()
            .flex()
            .flex_col()
            .mt(px(10.))
            .child(section_label(cx.t(&key("crash-section")), &c))
            .child(setting_row(
                cx.t(&key("crash-thread-title")),
                cx.t_fmt(&key("crash-thread-description"), &[("seconds", &CRASH_DELAY.as_secs().to_string())]),
                &c,
                pills(
                    "crash-thread",
                    &[(false, cx.t(&key("thread-main"))), (true, cx.t(&key("thread-worker")))],
                    self.crash_on_worker,
                    cx.listener(|this, worker: &bool, _, cx| {
                        this.crash_on_worker = *worker;
                        cx.notify();
                    }),
                    cx,
                ),
            ))
            .children(crash_rows)
            .child(divider(&c))
            .child(section_label(cx.t(&key("report-section")), &c))
            .child(setting_row(
                cx.t(&key("sample-title")),
                cx.t(&key("sample-description")),
                &c,
                Button::new("sample-report", cx.t(&key("sample-button")))
                    .small()
                    .on_click(|_, _, cx| crate::crash_report::show_sample(cx))
                    .into_any_element(),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("pending-title")),
                cx.t(&key("pending-description")),
                &c,
                Button::new("pending-crashes", cx.t(&key("pending-button")))
                    .small()
                    .on_click(|_, _, cx| crate::crash_report::offer_pending(cx))
                    .into_any_element(),
            ))
            .child(section_label(cx.t(&key("live-section")), &c))
            .child(setting_row(
                cx.t(&key("fps-title")),
                cx.t(&key("fps-description")),
                &c,
                tri_pills("hud", d.fps_hud, |s, v| s.fps_hud = v, |cx| cx.refresh_windows(), cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("mail-sim-title")),
                cx.t(&key("mail-sim-description")),
                &c,
                pills(
                    "mail-sim",
                    &MailSim::ALL.map(|m| (m, m.label(&locale))),
                    d.mail_sim,
                    cx.listener(|_, m: &MailSim, _, cx| {
                        let m = *m;
                        Settings::update(cx, move |s| s.debug.mail_sim = m);
                        cx.notify();
                    }),
                    cx,
                ),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("export-fallback-title")),
                cx.t(&key("export-fallback-description")),
                &c,
                self.debug_toggle("save-fallback", d.save_fallback, |s, v| s.save_fallback = v, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("restrict-logs-title")),
                cx.t(&key("restrict-logs-description")),
                &c,
                tri_pills("restrict-logs", d.restrict_log_targets, |s, v| s.restrict_log_targets = v, |_| {}, cx),
            ))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("manifest-title")),
                cx.t(&key("manifest-description")),
                &c,
                div().w(px(260.)).child(Field::new("debug-manifest", &self.manifest_input).paints_background(true)).into_any_element(),
            ))
            .child(section_label(cx.t(&key("restart-section")), &c))
            .child(restart_hint(cx.t(&key("restart-hint")), &c))
            .child(setting_row(cx.t(&key("view-cache-title")), cx.t(&key("view-cache-description")), &c,
                self.debug_toggle("view-cache", d.view_cache, |s, v| s.view_cache = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("view-cache-verify-title")), cx.t(&key("view-cache-verify-description")), &c,
                self.debug_toggle("view-cache-verify", d.view_cache_verify, |s, v| s.view_cache_verify = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("layout-retain-title")), cx.t(&key("layout-retain-description")), &c,
                self.debug_toggle("layout-retain", d.layout_retain, |s, v| s.layout_retain = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("layout-verify-title")), cx.t(&key("layout-verify-description")), &c,
                self.debug_toggle("layout-verify", d.layout_verify, |s, v| s.layout_verify = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("taffy-single-title")), cx.t(&key("taffy-single-description")), &c,
                self.debug_toggle("taffy-single", d.taffy_single_way, |s, v| s.taffy_single_way = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("no-glass-title")), cx.t(&key("no-glass-description")), &c,
                self.debug_toggle("no-glass", d.no_glass, |s, v| s.no_glass = v, cx)))
            .child(divider(&c))
            .child(setting_row(cx.t(&key("no-native-title")), cx.t(&key("no-native-description")), &c,
                self.debug_toggle("no-native", d.no_native, |s, v| s.no_native = v, cx)))
            .child(divider(&c))
            .child(setting_row(
                cx.t(&key("skin-title")),
                cx.t(&key("skin-description")),
                &c,
                pills(
                    "skin",
                    &SkinOverride::ALL.map(|k| (k, k.label(&locale))),
                    d.skin,
                    cx.listener(|_, k: &SkinOverride, _, cx| {
                        let k = *k;
                        Settings::update(cx, move |s| s.debug.skin = k);
                        cx.notify();
                    }),
                    cx,
                ),
            ))
    }

    fn debug_toggle(
        &self,
        id: &'static str,
        value: bool,
        set: impl Fn(&mut DebugSettings, bool) + 'static,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        Toggle::new(id, value)
            .on_change(cx.processor(move |_, next: bool, _window, cx| {
                Settings::update(cx, |s| set(&mut s.debug, next));
                cx.notify();
            }))
            .into_any_element()
    }

    /// Ask before crashing on purpose — it ends the app and drops any edit still in the autosave debounce.
    fn confirm_crash(&mut self, kind: Kind, window: &mut Window, cx: &mut Context<Self>) {
        let thread = if kind == Kind::Borrow || !self.crash_on_worker { Thread::Main } else { Thread::NamedWorker };
        let locale = cx.global::<crate::i18n::Locale>().clone();
        let thread_name = locale.t(&key(if thread == Thread::Main { "thread-main" } else { "thread-worker" }));
        let detail = cx.t_fmt(
            &key("confirm-detail"),
            &[
                ("kind", kind.label(&locale).as_ref()),
                ("thread", thread_name.as_ref()),
                ("seconds", &CRASH_DELAY.as_secs().to_string()),
            ],
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            &cx.t(&key("confirm-title")),
            Some(&detail),
            &[
                gpui_kit::PromptButton::new(cx.t(&key("confirm-button"))),
                gpui_kit::PromptButton::Cancel(cx.t(&key("confirm-cancel")).into()),
            ],
            cx,
        );
        cx.spawn_in(window, async move |_, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            cx.update(|_, cx| crash::trigger(kind, thread, CRASH_DELAY, cx)).ok();
        })
        .detach();
    }
}

fn restart_hint(text: SharedString, c: &crate::theme::PaletteColors) -> impl IntoElement {
    div().py(px(6.)).text_size(px(12.)).text_color(c.muted_foreground).child(text)
}

/// The catalog key `settings.debug.<name>`.
fn key(name: &str) -> String {
    format!("settings.debug.{name}")
}

fn tri_pills(
    id: &'static str,
    current: Tri,
    set: impl Fn(&mut DebugSettings, Tri) + 'static,
    after: impl Fn(&mut App) + 'static,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let set = Rc::new(set);
    let after = Rc::new(after);
    let locale = cx.global::<crate::i18n::Locale>().clone();
    pills(
        id,
        &Tri::ALL.map(|t| (t, t.label(&locale))),
        current,
        cx.listener(move |_, t: &Tri, _, cx| {
            let t = *t;
            let set = set.clone();
            Settings::update(cx, move |s| set(&mut s.debug, t));
            after(cx);
            cx.notify();
        }),
        cx,
    )
}

/// A small segmented switch (same look as the log-level picker).
fn pills<T: Copy + PartialEq + 'static>(
    id: &'static str,
    options: &[(T, SharedString)],
    current: T,
    on_pick: impl Fn(&T, &mut Window, &mut App) + 'static,
    cx: &mut Context<SettingsWindow>,
) -> AnyElement {
    let c = cx.theme().color;
    let radius = cx.theme().skin.radius_control_px();
    let inset = cx.theme().skin.radius_control_inset_px();
    let on_pick = Rc::new(on_pick);
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
        .children(options.iter().cloned().enumerate().map(|(i, (value, label))| {
            let selected = value == current;
            let on_pick = on_pick.clone();
            div()
                .id(SharedString::from(format!("{id}-{i}")))
                .flex()
                .items_center()
                .justify_center()
                .h(px(26.))
                .px(px(9.))
                .rounded(inset)
                .text_size(px(12.))
                .when(selected, |el| el.bg(c.surface).shadow_xs().text_color(c.foreground))
                .when(!selected, |el| {
                    el.text_color(c.muted_foreground).cursor_pointer().hover(|el| el.text_color(c.foreground))
                })
                .child(label)
                .on_click(move |_, window, cx| on_pick(&value, window, cx))
        }))
        .into_any_element()
}
