//! The countdown before a *simulated* crash (the hidden Debugging tab): every window dims, the settings window shows
//! a big "3 … 2 … 1" with a small Cancel button, and everything else is inert until the countdown ends or is
//! cancelled.
//!
//! State is one global ([`Slot`]). Each window's root paints [`overlay`] as its last child: a full-window layer that
//! **occludes** the pointer, above dialogs and popovers. Keys are swallowed by an `intercept_keystrokes` interceptor held
//! for the duration (Esc cancels), which also stops typing and shortcuts. The native menu bar is outside the windows and
//! stays reachable — this is a developer aid, not a lock screen.
//!
//! The fade-in and the per-second number pop go through `with_animation`, so they honour "reduce motion" like every other
//! animation here.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gpui_kit::{
    Animation, AnimationExt, AnyElement, App, Global, InteractiveElement, IntoElement, ParentElement, Styled, Subscription,
    Window, deferred, div, ease_out_quint, px,
};

use crate::components::{Button, ButtonTone};
use crate::debug::crash::{self, Kind, Thread};
use crate::i18n::ActiveLocale;
use crate::theme::ActiveTheme;

/// Above dialogs (10), sheets (50) and popovers (100).
const SCRIM_PRIORITY: usize = 200;
const CONTROLS_PRIORITY: usize = 210;
/// Side of the fixed square the big number lives in (≥ its largest animated size).
const NUMBER_BOX: f32 = 200.;

struct Countdown {
    id: u64,
    ends: Instant,
    kind: Kind,
    thread: Thread,
    /// The number last painted, so windows are refreshed only when it changes.
    shown: u64,
    _keys: Subscription,
}

#[derive(Default)]
struct Slot(Option<Countdown>);

impl Global for Slot {}

/// What a window paints while the countdown runs.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Just the dimming, pointer-blocking layer.
    Scrim,
    /// The window that shows the number and the Cancel button (the settings window). In a sheet the shell's own scrim
    /// already dims everything, so only the controls are added.
    Controller,
}

/// Is a countdown running?
pub fn active(cx: &App) -> bool {
    cx.try_global::<Slot>().is_some_and(|s| s.0.is_some())
}

fn remaining(cx: &App) -> Option<Duration> {
    let c = cx.try_global::<Slot>()?.0.as_ref()?;
    Some(c.ends.saturating_duration_since(Instant::now()))
}

/// The number shown for `left`: 3 for (2, 3], 2 for (1, 2], 1 for (0, 1].
fn shown_for(left: Duration) -> u64 {
    (left.as_secs_f64().ceil() as u64).max(1)
}

/// Start the countdown; when it reaches zero the crash is triggered. Replaces one already running.
pub fn start(kind: Kind, thread: Thread, delay: Duration, cx: &mut App) {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    log::warn!("crash simulation countdown: {kind:?} on {thread:?} in {delay:?}");

    // Swallow every key while it runs (typing, shortcuts); Esc cancels.
    let keys = cx.intercept_keystrokes(|event, _window, cx| {
        if event.keystroke.key == "escape" {
            cancel(cx);
        }
        cx.stop_propagation();
    });
    cx.default_global::<Slot>().0 =
        Some(Countdown { id, ends: Instant::now() + delay, kind, thread, shown: shown_for(delay), _keys: keys });
    cx.refresh_windows();

    cx.spawn(async move |cx| {
        loop {
            cx.background_executor().timer(Duration::from_millis(50)).await;
            let over = cx.update(|cx| tick(id, cx));
            if over {
                break;
            }
        }
    })
    .detach();
}

/// Stop the countdown (the Cancel button, Esc). Nothing crashes.
pub fn cancel(cx: &mut App) {
    if cx.default_global::<Slot>().0.take().is_some() {
        log::info!("crash simulation countdown cancelled");
        cx.refresh_windows();
    }
}

/// `true` once this countdown is finished (fired or cancelled / replaced).
fn tick(id: u64, cx: &mut App) -> bool {
    let Some(left) = remaining(cx) else { return true };
    if cx.try_global::<Slot>().and_then(|s| s.0.as_ref()).map(|c| c.id) != Some(id) {
        return true;
    }
    if left.is_zero() {
        let Some(done) = cx.default_global::<Slot>().0.take() else { return true };
        // The windows stay dimmed until the process dies; release the key interceptor first so nothing lingers if the
        // simulated fault is somehow survived.
        drop(done._keys);
        cx.refresh_windows();
        crash::trigger(done.kind, done.thread, Duration::ZERO, cx);
        return true;
    }
    let shown = shown_for(left);
    let changed = cx.default_global::<Slot>().0.as_mut().is_some_and(|c| std::mem::replace(&mut c.shown, shown) != shown);
    if changed {
        cx.refresh_windows();
    }
    false
}

/// The layer for one window, or `None` when no countdown is running. Paint it as the window root's last child.
pub fn overlay(window: &Window, cx: &App, mode: Mode) -> Option<AnyElement> {
    let left = remaining(cx)?;
    let c = cx.theme().color;
    let in_sheet = mode == Mode::Controller && crate::sheet::enabled();
    let fade = cx.theme().skin.motion(Duration::from_millis(320));
    let pop = cx.theme().skin.motion(Duration::from_millis(650));
    let _ = window;

    // Darker than the modal scrim: the whole app is meant to read as "paused".
    let dim = gpui_kit::Hsla { a: (c.overlay.a + 0.28).min(0.86), ..c.overlay };

    let controls = (mode == Mode::Controller).then(|| {
        let n = shown_for(left);
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap(px(6.))
            .child(
                div()
                    .text_size(px(15.))
                    .text_color(c.primary_foreground)
                    .child(cx.t("settings.debug.countdown-title")),
            )
            .child(
                // Restarts (same id ⇒ same animation; a new number ⇒ a new one) each second: pops in large and settles.
                // Fixed box: the number's size animates inside it, so the title and Cancel never move.
                div()
                    .w(px(NUMBER_BOX))
                    .h(px(NUMBER_BOX))
                    .flex()
                    .items_center()
                    .justify_center()
                    .overflow_hidden()
                    .child(
                        div()
                            .id(("crash-countdown-number", n))
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .text_color(c.primary_foreground)
                            .child(n.to_string())
                            .with_animation(
                                ("crash-countdown-number-anim", n),
                                Animation::new(pop).with_easing(ease_out_quint()),
                                |el, t| {
                                    let size = 190. - 60. * t;
                                    el.text_size(px(size)).line_height(px(size)).opacity(0.25 + 0.75 * t)
                                },
                            ),
                    ),
            )
            .child(
                Button::new("crash-countdown-cancel", cx.t("settings.debug.countdown-cancel"))
                    .small()
                    .tone(ButtonTone::Secondary)
                    .on_click(|_, _, cx| cancel(cx)),
            )
            .child(
                div()
                    .text_size(px(11.))
                    .text_color(c.primary_foreground)
                    .opacity(0.7)
                    .child(cx.t("settings.debug.countdown-hint")),
            )
    });

    let mut layer = div().absolute().inset_0().flex().items_center().justify_center();
    if !in_sheet {
        // `occlude`: nothing beneath sees the pointer. (In a sheet the shell's own scrim does this.)
        layer = layer.occlude().bg(dim);
    }
    let layer = layer.children(controls).with_animation(
        "crash-countdown-fade",
        Animation::new(fade).with_easing(ease_out_quint()),
        |el, t| el.opacity(t),
    );
    Some(
        deferred(layer)
            .with_priority(if mode == Mode::Controller { CONTROLS_PRIORITY } else { SCRIM_PRIORITY })
            .into_any_element(),
    )
}
