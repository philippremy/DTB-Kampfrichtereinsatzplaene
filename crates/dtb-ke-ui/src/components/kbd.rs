//! A keyboard-shortcut chip.
//!
//! Renders a keystroke as a **single block** per chord — `⌘⇧A`, not three
//! separate keycaps. On macOS the modifiers are their Apple glyphs (⌘ ⌥ ⇧ ⌃);
//! elsewhere they are words joined with `+` (`Ctrl+Shift+A`). A chord *sequence*
//! (`⌃X ⌃S`) shows one block per chord with a thin gap.

use gpui::{
    App, IntoElement, Keystroke, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px,
};

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
                        .map(|ks| block(chord_label(&ks))),
                )
            })
    }
}

/// The single-block label for one chord.
fn chord_label(ks: &Keystroke) -> String {
    let mut out = String::new();
    let m = &ks.modifiers;

    if cfg!(target_os = "macos") {
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
        out.push_str(&key_label(&ks.key));
    } else {
        let mut parts: Vec<&str> = Vec::new();
        if m.control {
            parts.push("Strg");
        }
        if m.alt {
            parts.push("Alt");
        }
        if m.shift {
            parts.push("Umschalt");
        }
        if m.platform {
            parts.push(platform_word());
        }
        if m.function {
            parts.push("Fn");
        }
        let key = key_label(&ks.key);
        parts.push(&key);
        out = parts.join("+");
    }
    out
}

fn platform_word() -> &'static str {
    if cfg!(target_os = "windows") {
        "Win"
    } else {
        "Super"
    }
}

/// Human-readable name for a single key.
fn key_label(key: &str) -> String {
    let mac = cfg!(target_os = "macos");
    let named = match key {
        "backspace" => Some(if mac { "⌫" } else { "Rück" }),
        "delete" => Some(if mac { "⌦" } else { "Entf" }),
        "enter" | "return" => Some(if mac { "⏎" } else { "Enter" }),
        "tab" => Some(if mac { "⇥" } else { "Tab" }),
        "escape" => Some(if mac { "⎋" } else { "Esc" }),
        "space" => Some(if mac { "␣" } else { "Leer" }),
        "up" => Some("↑"),
        "down" => Some("↓"),
        "left" => Some("←"),
        "right" => Some("→"),
        "pageup" => Some(if mac { "⇞" } else { "Bild↑" }),
        "pagedown" => Some(if mac { "⇟" } else { "Bild↓" }),
        "home" => Some(if mac { "↖" } else { "Pos1" }),
        "end" => Some(if mac { "↘" } else { "Ende" }),
        _ => None,
    };
    if let Some(named) = named {
        return named.to_string();
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
pub fn plain(keystroke: &str) -> String {
    keystroke
        .split_whitespace()
        .filter_map(|chord| Keystroke::parse(chord).ok())
        .map(|ks| chord_label(&ks))
        .collect::<Vec<_>>()
        .join(" ")
}
