//! "Raw dump": every stream of the dump on the left, the selected one as text on the right.

use dtb_ke_debugger::rawdump::{self, StreamEntry};
use dtb_ke_ui::components::icon::Icon;
use dtb_ke_ui::components::{Button, ButtonTone};
use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::base::Scrollbar;
use gpui_kit::{
    AnyElement, ClipboardItem, Context, InteractiveElement, IntoElement, ParentElement,
    ScrollHandle, SharedString, StatefulInteractiveElement, Styled, UniformListScrollHandle, div,
    px,
};

use super::DebuggerWindow;
use super::widgets::{self, TextBlock, section_title, selectable, text_list};

#[derive(Default)]
pub struct State {
    streams: Option<Vec<StreamEntry>>,
    selected: Option<usize>,
    block: Option<TextBlock>,
    list_scroll: ScrollHandle,
    body_scroll: UniformListScrollHandle,
}

impl DebuggerWindow {
    fn select_stream(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let Some(entry) = self.raw.streams.as_ref().and_then(|s| s.get(ix)) else {
            return;
        };
        let text = rawdump::stream_text(&session.opened.dump, entry.type_id);
        self.raw.selected = Some(ix);
        self.raw.block = Some(TextBlock::new(text));
        cx.notify();
    }

    pub(super) fn render_raw(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();

        if self.raw.streams.is_none() {
            let Some(session) = &self.session else {
                return div().into_any_element();
            };
            self.raw.streams = Some(rawdump::streams(&session.opened.dump));
        }
        if self.raw.selected.is_none() {
            // Start on the exception stream, or the first one.
            let first = self.raw.streams.as_ref().and_then(|s| {
                s.iter()
                    .position(|e| e.name == "ExceptionStream")
                    .or(if s.is_empty() { None } else { Some(0) })
            });
            if let Some(ix) = first {
                self.select_stream(ix, cx);
            }
        }
        let streams = self.raw.streams.clone().unwrap_or_default();

        let list =
            div()
                .flex()
                .flex_col()
                .size_full()
                .bg(c.chrome)
                .child(section_title(format!("Streams ({})", streams.len()), &c))
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h(px(0.))
                        .child(Scrollbar::vertical(&self.raw.list_scroll))
                        .child(
                            div()
                                .id("streams")
                                .size_full()
                                .overflow_y_scroll()
                                .track_scroll(&self.raw.list_scroll)
                                .px(px(6.))
                                .pb(px(8.))
                                .children(streams.iter().map(|s| {
                                    let ix = s.index;
                                    let selected = self.raw.selected == Some(ix);
                                    selectable(("stream", ix), selected, &c, radius)
                                        .flex()
                                        .flex_col()
                                        .gap(px(2.))
                                        .px(px(10.))
                                        .py(px(6.))
                                        .child(
                                            div()
                                                .text_size(px(12.5))
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_color(if selected {
                                                    c.primary
                                                } else if s.understood {
                                                    c.foreground
                                                } else {
                                                    c.muted_foreground
                                                })
                                                .child(SharedString::from(s.name.clone())),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.5))
                                                .overflow_hidden()
                                                .whitespace_nowrap()
                                                .text_ellipsis()
                                                .text_color(c.muted_foreground)
                                                .child(SharedString::from(format!(
                                                    "{:#010x} · {} · {} B",
                                                    s.type_id, s.vendor, s.size
                                                ))),
                                        )
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.select_stream(ix, cx)
                                        }))
                                })),
                        ),
                );

        let body: AnyElement = match &self.raw.block {
            Some(block) => text_list("raw-text", block, &self.raw.body_scroll, cx),
            None => widgets::placeholder("No stream selected.", &c),
        };
        let copy_enabled = self.raw.block.is_some();
        let detail = div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .flex_col()
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .px(px(14.))
                    .h(px(36.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        Button::new("raw-copy", "Kopieren")
                            .small()
                            .tone(ButtonTone::Secondary)
                            .leading_icon(Icon::Copy)
                            .disabled(!copy_enabled)
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(b) = &this.raw.block {
                                    cx.write_to_clipboard(ClipboardItem::new_string(
                                        b.text.to_string(),
                                    ));
                                }
                            })),
                    ),
            )
            .child(body);

        self.with_sidebar(list.into_any_element(), detail.into_any_element(), cx)
    }
}
