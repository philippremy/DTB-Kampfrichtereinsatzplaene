//! Small shared building blocks for the viewer's tabs.

use std::ops::Range;
use std::sync::Arc;

use dtb_ke_ui::theme::{ActiveTheme, PaletteColors};
use gpui_kit::base::{Scrollbar, SelectableText};
use gpui_kit::{
    AnyElement, App, Hsla, InteractiveElement, IntoElement, ListHorizontalSizingBehavior,
    ParentElement, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    UniformListScrollHandle, div, prelude::FluentBuilder, px, uniform_list,
};

/// A block of text split into lines once, so a `uniform_list` can lay out only the visible ones.
#[derive(Clone)]
pub struct TextBlock {
    pub text: Arc<str>,
    lines: Arc<[Range<usize>]>,
    widest: usize,
}

impl TextBlock {
    pub fn new(text: String) -> Self {
        let text: Arc<str> = Arc::from(text);
        let b = text.as_bytes();
        let mut lines = Vec::new();
        let mut start = 0;
        for (i, &byte) in b.iter().enumerate() {
            if byte == b'\n' {
                let end = if i > start && b[i - 1] == b'\r' {
                    i - 1
                } else {
                    i
                };
                lines.push(start..end);
                start = i + 1;
            }
        }
        if start < b.len() {
            lines.push(start..b.len());
        }
        if lines.is_empty() {
            lines.push(0..0);
        }
        let widest = lines
            .iter()
            .enumerate()
            .max_by_key(|(_, r)| text[(*r).clone()].chars().count())
            .map(|(i, _)| i)
            .unwrap_or(0);
        Self {
            text,
            lines: Arc::from(lines),
            widest,
        }
    }

    pub fn len(&self) -> usize {
        self.lines.len()
    }

    /// Index of the longest line (what a horizontally scrolling list measures its extent from).
    pub fn widest(&self) -> usize {
        self.widest
    }

    /// Line `ix` (0-based), without its newline.
    pub fn line(&self, ix: usize) -> &str {
        self.lines
            .get(ix)
            .map(|r| &self.text[r.clone()])
            .unwrap_or("")
    }
}

/// The text as monospace lines, scrolling on both axes, with a vertical scrollbar.
pub fn text_list(
    id: &'static str,
    block: &TextBlock,
    scroll: &UniformListScrollHandle,
    cx: &App,
) -> AnyElement {
    let c = cx.theme().color;
    let mono = cx.theme().skin.mono_font_family();
    let (text, lines, widest) = (block.text.clone(), block.lines.clone(), block.widest);
    let list = uniform_list(id, lines.len(), move |range, _w, _cx| {
        range
            .filter_map(|ix| lines.get(ix).cloned().map(|span| (ix, span)))
            .map(|(ix, span)| {
                div().whitespace_nowrap().min_h(px(15.)).child(
                    SelectableText::new((id, ix), text[span].to_string())
                        .document_order(ix as u64),
                )
            })
            .collect::<Vec<_>>()
    })
    .track_scroll(scroll)
    .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
    .with_width_from_item(Some(widest))
    .size_full()
    .px(px(14.))
    .py(px(10.))
    .font_family(mono)
    .text_size(px(11.5))
    .text_color(c.foreground);

    div()
        .relative()
        .flex_1()
        .min_h(px(0.))
        .min_w(px(0.))
        .child(list)
        .child(Scrollbar::vertical(scroll))
        .into_any_element()
}

/// How [`listing_rows`] lays out a label and its value.
#[derive(Clone, Copy)]
pub enum ListStyle {
    /// Label above the value — for narrow panes, where a side-by-side label column would squeeze the value.
    Stacked,
    /// Label in a fixed column, value beside it, left-aligned — for wide panes with long values.
    Inline,
    /// Label at the left edge, value right-aligned at the right edge — so short, fixed-width values (the
    /// registers) still span the pane however wide it is dragged.
    Spread,
}

/// Label / value rows, the label column dimmed — no scrolling of its own, so it can sit inside a larger
/// scroll area.
pub fn listing_rows(rows: Vec<(String, String)>, style: ListStyle, cx: &App) -> gpui_kit::Div {
    let c = cx.theme().color;
    let mono = cx.theme().skin.mono_font_family();
    div()
        .px(px(14.))
        .py(px(8.))
        .flex()
        .flex_col()
        .gap(px(if matches!(style, ListStyle::Stacked) {
            8.
        } else {
            3.
        }))
        .children(rows.into_iter().enumerate().map(|(i, (k, v))| {
            let order = i as u64 * 2;
            let label = div()
                .flex_none()
                .text_color(c.muted_foreground)
                .child(SelectableText::new(("row-label", i), k).document_order(order));
            let value = div()
                .min_w(px(0.))
                .overflow_hidden()
                .font_family(mono.clone())
                .child(SelectableText::new(("row-value", i), v).document_order(order + 1));
            match style {
                ListStyle::Stacked => div()
                    .flex()
                    .flex_col()
                    .gap(px(1.))
                    .child(label.text_size(px(10.5)))
                    .child(value.text_size(px(12.))),
                ListStyle::Inline => div()
                    .flex()
                    .gap(px(12.))
                    .text_size(px(12.))
                    .child(label.w(px(140.)))
                    .child(value.flex_1()),
                ListStyle::Spread => div()
                    .flex()
                    .justify_between()
                    .gap(px(12.))
                    .text_size(px(12.))
                    .child(label)
                    .child(value.flex_none()),
            }
        }))
}

/// [`listing_rows`] in a pane of its own that scrolls vertically when it overflows.
pub fn listing(
    id: &'static str,
    rows: Vec<(String, String)>,
    scroll: &ScrollHandle,
    style: ListStyle,
    cx: &App,
) -> AnyElement {
    div()
        .relative()
        .flex_1()
        .min_h(px(0.))
        .child(Scrollbar::vertical(scroll))
        .child(
            div()
                .id(id)
                .size_full()
                .overflow_y_scroll()
                .track_scroll(scroll)
                .child(listing_rows(rows, style, cx)),
        )
        .into_any_element()
}

pub fn section_title(text: impl Into<SharedString>, c: &PaletteColors) -> impl IntoElement {
    div()
        .flex_none()
        .h(px(30.))
        .px(px(14.))
        .flex()
        .items_center()
        .text_size(px(11.))
        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
        .text_color(c.muted_foreground)
        .child(text.into())
}

pub fn placeholder(text: impl Into<SharedString>, c: &PaletteColors) -> AnyElement {
    div()
        .p(px(24.))
        .text_size(px(13.))
        .text_color(c.muted_foreground)
        .child(text.into())
        .into_any_element()
}

pub fn tint(color: Hsla, a: f32) -> Hsla {
    Hsla { a, ..color }
}

/// A selectable list row (sidebar-style) — selected rows get the accent tint, others hover.
pub fn selectable(
    id: impl Into<gpui_kit::ElementId>,
    selected: bool,
    c: &PaletteColors,
    radius: gpui_kit::Pixels,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let c = *c;
    div()
        .id(id)
        .rounded(radius)
        .when(selected, |el| el.bg(tint(c.primary, 0.14)))
        .when(!selected, |el| {
            el.cursor_pointer()
                .hover(move |el| el.bg(tint(c.foreground, 0.06)))
        })
}
