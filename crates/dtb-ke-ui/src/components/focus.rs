//! Skin-specific treatments for control state (focus, selection).

use gpui::{Div, Hsla, Styled, div, px};

use crate::theme::{Appearance, FocusRing, SelectionStyle, Theme};

/// The border colour a bordered field uses, given whether it is focused and the
/// skin's focus treatment.
///
/// * `Outline` (macOS, Linux) — the whole 1px border switches to the accent
///   colour on focus.
/// * `Underline` (WinUI 3) — the border stays subtle; focus shows a bottom bar
///   instead (see [`focus_underline`]).
pub fn field_border_color(focused: bool, theme: &Theme) -> Hsla {
    if focused && matches!(theme.skin.focus_ring, FocusRing::Outline) {
        theme.color.ring
    } else {
        theme.color.border
    }
}

/// The WinUI-style 2px accent bar drawn inside the bottom edge of a focused
/// field. `None` for every other skin, or when the field is not focused. The
/// caller must render this as a child of a `relative` container.
pub fn focus_underline(focused: bool, theme: &Theme) -> Option<Div> {
    (focused && matches!(theme.skin.focus_ring, FocusRing::Underline)).then(|| {
        div()
            .absolute()
            .bottom_0()
            .left_0()
            .right_0()
            .h(px(2.))
            .bg(theme.color.ring)
    })
}

/// The `(fill, foreground)` a selected item uses, per skin selection style.
pub fn selection_fill(theme: &Theme) -> (Hsla, Hsla) {
    let c = &theme.color;
    match theme.skin.selection_style {
        SelectionStyle::GlassTint => (c.primary, gpui::white()),
        // A crisp pill against the chrome, matching real WinUI 3 (File
        // Explorer, Settings). In light mode `surface` (near-white) already
        // pops against `chrome` (a very light grey) — but the dark palette's
        // `surface` and `chrome` are only a couple of RGB points apart, so
        // the "pill" was nearly invisible there (worse still on Windows'
        // blurred backdrop, which further washes out the already-faint
        // contrast). `selection` is the palette's dedicated token for
        // exactly this ("sidebar-row selection base") and is meaningfully
        // lighter than `chrome` in dark mode — use it there instead.
        SelectionStyle::WinuiPill => match theme.appearance {
            Appearance::Light => (c.surface, c.foreground),
            Appearance::Dark => (c.selection, c.foreground),
        },
        // Linux / GNOME. The palette's `selection` token washed out badly
        // against `chrome` (barely any contrast in light mode), so tint the
        // accent directly instead — light stays legible under dark text, dark
        // under light text.
        SelectionStyle::SolidSubtle => {
            let alpha = match theme.appearance {
                Appearance::Light => 0.28,
                Appearance::Dark => 0.38,
            };
            (
                Hsla {
                    a: alpha,
                    ..c.primary
                },
                c.foreground,
            )
        }
    }
}
