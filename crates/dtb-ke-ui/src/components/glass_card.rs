//! A plain, always-resting rounded card surface — real Liquid Glass, clipped
//! to a scrollable viewport, on the native tier; a bordered/filled panel
//! everywhere else. Used by the detail view's spare-judges and remarks
//! sections, which (unlike [`crate::detail::judging_table::JudgingTableCard`])
//! never need a selection / conflict / drag ring on top, so they can share
//! this one wrapper instead of each re-deriving `crate::skin::glass`'s
//! resting-state fallback styling.
//!
//! Returns a plain [`Div`] (unpadded, per the glass-region convention — see
//! `crate::skin::glass`'s module doc comment) rather than a custom element,
//! so the caller keeps full `Styled`/`ParentElement` access: chain layout
//! (`.mx(..)`/`.mb(..)`) directly on it, and put the section's own
//! id/padding/interactive content in a child div.

use gpui_kit::{
    App, Bounds, Div, ParentElement, Pixels, SharedString, Styled, div, prelude::FluentBuilder,
};

use crate::skin::glass::{self, GlassRole};
use crate::theme::ActiveTheme;

/// `id` must be unique per window and stable for the card's lifetime.
/// `viewport` is the enclosing scroll pane's current window-relative bounds
/// (`None` before it's painted once, which falls back to the plain panel for
/// that one frame).
pub fn glass_card(
    id: impl Into<SharedString>,
    viewport: Option<Bounds<Pixels>>,
    cx: &mut App,
) -> Div {
    let theme = cx.theme();
    let c = theme.color;
    let radius = theme.skin.radius_lg_px();
    let show_glass = glass::active() && viewport.is_some();

    div()
        .relative()
        .rounded(radius)
        .when(show_glass, |el| {
            el.child(glass::region_in_viewport(
                id,
                GlassRole::Card,
                viewport.expect("checked by show_glass"),
                glass::DETAIL_SCROLL_GROUP,
                None,
            ))
        })
        .when(!show_glass, |el| {
            el.border_1().border_color(c.border).bg(c.surface)
        })
}
