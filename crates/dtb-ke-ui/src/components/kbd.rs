//! A keyboard-shortcut chip.
//!
//! Renders a keystroke as a **single block** per chord — `⌘⇧A`, not three
//! separate keycaps. On macOS the modifiers are their Apple glyphs (⌘ ⌥ ⇧ ⌃);
//! elsewhere they are words joined with `+` (`Ctrl+Shift+A`). A chord *sequence*
//! (`⌃X ⌃S`) shows one block per chord with a thin gap.

use gpui_kit::{
    App, IntoElement, Keystroke, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::i18n::ActiveLocale;
use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct Kbd {
    keystroke: SharedString,
    muted: bool,
    /// A placeholder look — dashed border, "—" glyph — for an unbound command.
    unbound: bool,
}

impl Kbd {
    pub fn new(keystroke: impl Into<SharedString>) -> Self {
        Self {
            keystroke: keystroke.into(),
            muted: false,
            unbound: false,
        }
    }

    /// Nothing is bound — draw a dashed placeholder.
    pub fn unbound() -> Self {
        Self {
            keystroke: SharedString::default(),
            muted: true,
            unbound: true,
        }
    }

    pub fn muted(mut self, muted: bool) -> Self {
        self.muted = muted;
        self
    }
}

impl RenderOnce for Kbd {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let radius = px((theme.skin.radius_control - 3.0).max(3.0));
        let fg = if self.muted {
            c.muted_foreground
        } else {
            c.surface_foreground
        };

        let block = |label: String| {
            div()
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .h(px(20.))
                .min_w(px(20.))
                .px(px(6.))
                .rounded(radius)
                .border_1()
                .border_color(c.border)
                .bg(c.chrome)
                .text_size(px(11.5))
                .text_color(fg)
                .child(label)
        };

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(3.))
            .when(self.unbound, |el| {
                el.child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .h(px(20.))
                        .px(px(8.))
                        .rounded(radius)
                        .border_1()
                        .border_dashed()
                        .border_color(c.border)
                        .text_size(px(11.5))
                        .text_color(c.muted_foreground)
                        .child("—"),
                )
            })
            .when(!self.unbound, |el| {
                el.children(
                    self.keystroke
                        .split_whitespace()
                        .filter_map(|chord| Keystroke::parse(chord).ok())
                        .map(|ks| block(chord_label(&ks, cx))),
                )
            })
    }
}

/// The single-block label for one chord.
fn chord_label(ks: &Keystroke, cx: &App) -> String {
    let mut out = String::new();
    let m = &ks.modifiers;

    if cfg!(any(target_os = "macos", target_os = "ios")) {
        // Apple order: fn ⌃ ⌥ ⇧ ⌘, glyphs butted together, then the key.
        if m.function {
            out.push_str("fn ");
        }
        if m.control {
            out.push('⌃');
        }
        if m.alt {
            out.push('⌥');
        }
        if m.shift {
            out.push('⇧');
        }
        if m.platform {
            out.push('⌘');
        }
        out.push_str(&key_label(&ks.key, cx));
    } else {
        let mut parts: Vec<String> = Vec::new();
        if m.control {
            parts.push(cx.t("kbd.modifier-ctrl").to_string());
        }
        if m.alt {
            parts.push(cx.t("kbd.modifier-alt").to_string());
        }
        if m.shift {
            parts.push(cx.t("kbd.modifier-shift").to_string());
        }
        if m.platform {
            parts.push(platform_word(cx));
        }
        if m.function {
            parts.push(cx.t("kbd.modifier-fn").to_string());
        }
        parts.push(key_label(&ks.key, cx));
        out = parts.join("+");
    }
    out
}

fn platform_word(cx: &App) -> String {
    let key = if cfg!(target_os = "windows") {
        "kbd.modifier-platform-windows"
    } else {
        "kbd.modifier-platform-other"
    };
    cx.t(key).to_string()
}

/// Human-readable name for a single key.
fn key_label(key: &str, cx: &App) -> String {
    let mac = cfg!(any(target_os = "macos", target_os = "ios"));
    let named: Option<String> = match key {
        "backspace" => Some(if mac {
            "⌫".to_string()
        } else {
            cx.t("kbd.key-backspace").to_string()
        }),
        "delete" => Some(if mac {
            "⌦".to_string()
        } else {
            cx.t("kbd.key-delete").to_string()
        }),
        "enter" | "return" => Some(if mac {
            "⏎".to_string()
        } else {
            cx.t("kbd.key-enter").to_string()
        }),
        "tab" => Some(if mac {
            "⇥".to_string()
        } else {
            cx.t("kbd.key-tab").to_string()
        }),
        "escape" => Some(if mac {
            "⎋".to_string()
        } else {
            cx.t("kbd.key-escape").to_string()
        }),
        "space" => Some(if mac {
            "␣".to_string()
        } else {
            cx.t("kbd.key-space").to_string()
        }),
        "up" => Some("↑".to_string()),
        "down" => Some("↓".to_string()),
        "left" => Some("←".to_string()),
        "right" => Some("→".to_string()),
        "pageup" => Some(if mac {
            "⇞".to_string()
        } else {
            cx.t("kbd.key-pageup").to_string()
        }),
        "pagedown" => Some(if mac {
            "⇟".to_string()
        } else {
            cx.t("kbd.key-pagedown").to_string()
        }),
        "home" => Some(if mac {
            "↖".to_string()
        } else {
            cx.t("kbd.key-home").to_string()
        }),
        "end" => Some(if mac {
            "↘".to_string()
        } else {
            cx.t("kbd.key-end").to_string()
        }),
        _ => None,
    };
    if let Some(named) = named {
        return named;
    }
    if key.len() == 1 {
        return key.to_uppercase();
    }
    // f1 → F1, etc.
    let mut chars = key.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// The plain-text form (for tooltips / `aria` later).
pub fn plain(keystroke: &str, cx: &App) -> String {
    keystroke
        .split_whitespace()
        .filter_map(|chord| Keystroke::parse(chord).ok())
        .map(|ks| chord_label(&ks, cx))
        .collect::<Vec<_>>()
        .join(" ")
}
