//! A button: label and/or icon, four tones, two sizes.

use gpui_kit::{
    App, ClickEvent, ElementId, Hsla, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    Rgba, SharedString, StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder,
    px, transparent_black,
};

use crate::components::icon::Icon;
use crate::theme::ActiveTheme;

type ClickHandler = Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>;

/// Alpha-composite `over` onto the opaque `base`, returning an opaque colour.
fn composite(base: Hsla, over: Hsla) -> Hsla {
    Hsla::from(Rgba::from(base).blend(Rgba::from(over)))
}

/// Lower the lightness of `base` by `amount` (0–1).
fn darken(base: Hsla, amount: f32) -> Hsla {
    Hsla {
        l: (base.l - amount).max(0.0),
        ..base
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonTone {
    /// Filled with the accent colour — the one primary action per view.
    Primary,
    /// Bordered, surface fill — the default.
    #[default]
    Secondary,
    /// No fill or border until hovered — toolbar / inline actions.
    Ghost,
    /// Filled with the critical colour — destructive actions.
    Danger,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ButtonSize {
    Xsmall,
    Small,
    #[default]
    Medium,
}

#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    label: Option<SharedString>,
    leading: Option<Icon>,
    tone: ButtonTone,
    size: ButtonSize,
    disabled: bool,
    round: bool,
    oval: bool,
    scale: f32,
    tooltip: Option<SharedString>,
    foreground: Option<Hsla>,
    on_click: Option<ClickHandler>,
}

impl Button {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: Some(label.into()),
            leading: None,
            tone: ButtonTone::default(),
            size: ButtonSize::default(),
            disabled: false,
            round: false,
            oval: false,
            scale: 1.0,
            tooltip: None,
            foreground: None,
            on_click: None,
        }
    }

    /// An icon-only button (square).
    pub fn icon(id: impl Into<ElementId>, icon: Icon) -> Self {
        Self {
            id: id.into(),
            label: None,
            leading: Some(icon),
            tone: ButtonTone::Ghost,
            size: ButtonSize::default(),
            disabled: false,
            round: false,
            oval: false,
            scale: 1.0,
            tooltip: None,
            foreground: None,
            on_click: None,
        }
    }

    pub fn tone(mut self, tone: ButtonTone) -> Self {
        self.tone = tone;
        self
    }

    pub fn small(mut self) -> Self {
        self.size = ButtonSize::Small;
        self
    }

    pub fn x_small(mut self) -> Self {
        self.size = ButtonSize::Xsmall;
        self
    }

    pub fn leading_icon(mut self, icon: Icon) -> Self {
        self.leading = Some(icon);
        self
    }

    /// A hover tooltip — give every icon-only button one (it is also its
    /// accessible name).
    pub fn tooltip(mut self, label: impl Into<SharedString>) -> Self {
        self.tooltip = Some(label.into());
        self
    }

    /// Overrides the label / icon colour (e.g. on an accent-filled surface).
    pub fn foreground(mut self, color: Hsla) -> Self {
        self.foreground = Some(color);
        self
    }

    /// Scales the icon by `scale` about its centre (the button's own box stays
    /// put) — the press bump of a [`crate::components::toolbar_group::ToolbarGroup`].
    pub fn scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// An icon-only button drawn as an oval rather than a circle: same icon,
    /// a little less padding above and below, more at the sides. Implies
    /// [`Self::round`]. For buttons inside a toolbar capsule.
    pub fn oval(mut self) -> Self {
        self.round = true;
        self.oval = true;
        self
    }

    /// Fully rounded (a circle for an icon-only button, a capsule otherwise).
    pub fn round(mut self) -> Self {
        self.round = true;
        self
    }

    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Box::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let c = &theme.color;
        let icon_only = self.label.is_none();

        let (height, pad_x, gap, text_size) = match self.size {
            ButtonSize::Xsmall => (px(20.), px(4.), px(4.), px(10.)),
            ButtonSize::Small => (px(24.), px(8.), px(4.), px(12.)),
            ButtonSize::Medium => (px(30.), px(12.), px(6.), px(13.)),
        };

        // Hover / active feedback: opaque fills darken; Secondary and Ghost use
        // a translucent ink "state layer" so it reads on any background — in
        // particular the toolbar, whose fill matches `chrome`.
        let layer = |alpha: f32| Hsla {
            a: alpha,
            ..c.foreground
        };
        let (fill, fg, border, hover_bg, active_bg) = match self.tone {
            ButtonTone::Primary => (
                c.primary,
                c.primary_foreground,
                c.primary,
                darken(c.primary, 0.06),
                darken(c.primary, 0.10),
            ),
            ButtonTone::Danger => (
                c.critical,
                c.destructive_foreground,
                c.critical,
                darken(c.critical, 0.06),
                darken(c.critical, 0.10),
            ),
            ButtonTone::Secondary => (
                c.surface,
                c.foreground,
                c.border,
                composite(c.surface, layer(0.05)),
                composite(c.surface, layer(0.10)),
            ),
            ButtonTone::Ghost => (
                transparent_black(),
                c.foreground,
                transparent_black(),
                layer(0.07),
                layer(0.12),
            ),
        };

        let fg = self.foreground.unwrap_or(fg);
        // An oval icon button is shorter than a round one and wider than tall.
        let (height, width) = if self.oval && icon_only {
            (px(28.), px(36.))
        } else {
            (height, height)
        };
        let mut el = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .gap(gap)
            .h(height)
            .when(self.round, |el| el.rounded_full())
            .when(!self.round, |el| {
                el.rounded(theme.skin.control_radius_px(height))
            })
            .border_1()
            .border_color(border)
            .bg(fill)
            .text_color(fg)
            .text_size(text_size)
            .when(icon_only, |el| el.w(width))
            .when(!icon_only, |el| el.px(pad_x));

        if let Some(label) = self.tooltip {
            el = el.tooltip(crate::components::tooltip::text_tooltip(label));
        }

        if self.disabled {
            el = el.opacity(0.45);
        } else {
            el = el
                .cursor_pointer()
                .hover(move |el| el.bg(hover_bg))
                .active(move |el| el.bg(active_bg));
            if let Some(handler) = self.on_click {
                el = el.on_click(move |ev, window, cx| handler(ev, window, cx));
            }
        }

        el.when_some(self.leading, |el, icon| {
            el.child(
                icon.size(px(match self.size {
                    ButtonSize::Xsmall => 11.,
                    ButtonSize::Small => 13.,
                    ButtonSize::Medium => 15.,
                } * self.scale))
                .color(fg),
            )
        })
        .when_some(self.label, |el, label| el.child(label))
    }
}
