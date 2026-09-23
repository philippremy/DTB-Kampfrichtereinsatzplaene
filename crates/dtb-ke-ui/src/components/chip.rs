//! A small non-interactive pill: discipline tags, the save-state indicator, the
//! meeting-time chip, conflict counts.

use gpui_kit::{
    App, Hsla, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div,
    prelude::FluentBuilder, px,
};

use crate::components::icon::Icon;
use crate::theme::{ActiveTheme, Theme};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChipTone {
    #[default]
    Neutral,
    Accent,
    Ok,
    Warn,
    Critical,
}

impl ChipTone {
    /// `(fill, foreground, border)` — every chip carries a hairline border so it
    /// reads as a pill even on a same-coloured surface (e.g. the toolbar).
    fn colors(self, theme: &Theme) -> (Hsla, Hsla, Hsla) {
        let c = &theme.color;
        let tint = |base: Hsla, a: f32| Hsla { a, ..base };
        match self {
            ChipTone::Neutral => (c.surface, c.muted_foreground, c.border),
            ChipTone::Accent => (c.accent_soft, c.primary, tint(c.primary, 0.28)),
            ChipTone::Ok => (tint(c.ok, 0.14), c.ok, tint(c.ok, 0.32)),
            ChipTone::Warn => (tint(c.warn, 0.14), c.warn, tint(c.warn, 0.32)),
            ChipTone::Critical => (tint(c.critical, 0.14), c.critical, tint(c.critical, 0.32)),
        }
    }
}

#[derive(IntoElement)]
pub struct Chip {
    label: SharedString,
    tone: ChipTone,
    leading: Option<Icon>,
    mono: bool,
}

impl Chip {
    pub fn new(label: impl Into<SharedString>) -> Self {
        Self {
            label: label.into(),
            tone: ChipTone::default(),
            leading: None,
            mono: false,
        }
    }

    pub fn tone(mut self, tone: ChipTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn leading_icon(mut self, icon: Icon) -> Self {
        self.leading = Some(icon);
        self
    }

    /// Render the label in the skin's monospace family (meeting times, codes).
    pub fn mono(mut self) -> Self {
        self.mono = true;
        self
    }
}

impl RenderOnce for Chip {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fill, fg, border) = self.tone.colors(theme);
        let mono = self.mono.then(|| theme.skin.mono_font_family());

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(4.))
            .h(px(20.))
            .px(px(8.))
            .rounded_full()
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_color(fg)
            .text_size(px(10.5))
            .when_some(mono, |el, family| el.font_family(family))
            .when_some(self.leading, |el, icon| {
                el.child(icon.size(px(12.)).color(fg))
            })
            .child(self.label)
    }
}
