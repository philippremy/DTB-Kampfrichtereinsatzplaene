//! The trailing free-form remarks section.
//!
//! A real WYSIWYG rich-text field on top of `gpui-base`'s `EditorState` (its
//! code-editor kind, used here with no language / line numbers / folding —
//! just the multi-line editing engine plus its `TextDecoration` API). A small
//! **F / K / U** toolbar plus a text-color picker toggle bold / italic /
//! underline / color over the current selection.
//!
//! Formatting is tracked per-character in [`styles`](RemarksSection::styles),
//! a `Vec<CharStyle>` parallel to the editor's text (one entry per Unicode
//! scalar value). On every `InputEvent::Change` [`RemarksSection::resync_styles`]
//! diffs the old and new text by common prefix/suffix — the same technique
//! plain-text editors use to track annotations across edits — and carries the
//! surrounding style onto whatever was inserted. This sidesteps needing to
//! read a decoration's style back out of `TextDecorationCollection` (which
//! only exposes ranges, by design — the collection is meant to be a *view*
//! the app renders from data it already owns, not a store the app reads back).
//! [`render_decorations`](RemarksSection::render_decorations) coalesces
//! `styles` into runs and replaces the (single) decoration collection
//! wholesale from that, so the collection is always a deterministic function
//! of `styles`, never a second source of truth.

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui::{
    AbsoluteLength, AppContext, Bounds, Context, Entity, FontStyle, FontWeight, HighlightStyle,
    Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement, Pixels, Render, Rgba,
    StatefulInteractiveElement, Styled, Subscription, UnderlineStyle, Window, anchored, canvas,
    deferred, div, hsla, linear_color_stop, linear_gradient, point, prelude::FluentBuilder, px,
    relative,
};
use gpui_base::input::{Editor, EditorState, InputEvent, TextDecoration};
use gpui_base::slider::SliderState;
use gpui_base::{
    ColorPickerEvent, ColorPickerState, ColorSwatch, Slider, SliderIndicator, SliderThumb,
    SliderTrack,
};

use crate::components::field::Field;
use crate::i18n::ActiveLocale;
use crate::model::{Paragraph, RichText, Run};
use crate::store::RemarksEditor;
use crate::theme::ActiveTheme;

/// A handful of preset text colors, shown as quick swatches above the sliders.
const PRESETS: [u32; 6] = [0xdc2626, 0xd97706, 0x16a34a, 0x2563eb, 0x7c3aed, 0x171717];

pub struct RemarksSection {
    remarks: Option<Entity<RemarksEditor>>,
    input: Entity<EditorState>,
    /// The single decoration collection this section owns; rebuilt wholesale
    /// from `styles` on every change. `None` only before the first render
    /// (creating it needs `&mut Context<EditorState>`, wired up in `new`).
    decorations: gpui_base::input::TextDecorationCollection,
    /// One entry per `char` of the editor's current text — the authoritative
    /// formatting state. Kept in lock-step with `last_text`.
    styles: Vec<CharStyle>,
    /// The editor's text as of the last resync, for prefix/suffix diffing.
    last_text: String,
    /// A style override for the *next* characters typed, set by a toolbar
    /// action taken with no selection (Word's "format the insertion point"
    /// convention: nothing existing changes, but everything typed from then
    /// on carries it). Stays in effect until another such action changes or
    /// clears it — there is no cursor-move event to invalidate it on, so
    /// unlike Word it does not reset just by clicking elsewhere.
    pending_style: Option<CharStyle>,
    color_picker: Entity<ColorPickerState>,
    color_open: bool,
    /// The char range the color popover applies to, captured once when it
    /// opens. `None` means it applies to `pending_style` instead (opened
    /// with no selection).
    color_target: Option<Range<usize>>,
    /// Absolute bounds of the color trigger, captured during prepaint so the
    /// popover can float above the detail scroll container.
    color_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    _subs: Vec<Subscription>,
}

impl RemarksSection {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let placeholder = cx.t("detail.remarks.placeholder");
        let input = cx.new(|cx| {
            EditorState::new(window, cx)
                .placeholder(placeholder)
                .line_number(false)
                .folding(false)
                // Code-editor mode reserves scroll space below the last line
                // by default — `None` means *half the viewport height* (see
                // `InputBaseState::scroll_beyond_last_line`), which is what
                // actually produced the growing dead zone at the bottom: the
                // wrapper's own height feeds back into that viewport-relative
                // reservation, so taller wrapper → more reserved space →
                // taller wrapper. `Some(0)` removes it outright.
                .scroll_beyond_last_line(Some(0))
        });
        let decorations = input.update(cx, |state, cx| {
            state.create_decorations_collection(Vec::new(), cx)
        });
        let color_picker = cx.new(|cx| ColorPickerState::new(window, cx));

        let mut subs = vec![
            cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.resync_styles(cx);
                }
            }),
            // Generic entity-level notify, not just `InputEvent::Change`: the
            // toolbar buttons need to re-render on a *selection*-only change
            // too (clicking/dragging/arrow-keying to a new selection with no
            // text edit), which `EditorState` still `cx.notify()`s on for its
            // own cursor/selection-highlight repaint but never turns into an
            // `InputEvent`.
            cx.observe(&input, |_, _, cx| cx.notify()),
        ];
        subs.push(cx.subscribe_in(
            &color_picker,
            window,
            |this, _, event: &ColorPickerEvent, window, cx| {
                let ColorPickerEvent::Change(color) = event;
                this.apply_color(color.map(hex_string), window, cx);
            },
        ));

        Self {
            remarks: None,
            input,
            decorations,
            styles: Vec::new(),
            last_text: String::new(),
            pending_style: None,
            color_picker,
            color_open: false,
            color_target: None,
            color_bounds: Rc::new(Cell::new(None)),
            _subs: subs,
        }
    }

    pub fn rebind(
        &mut self,
        remarks: Entity<RemarksEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (text, styles) = flatten(remarks.read(cx).text());
        self.remarks = Some(remarks);
        // Set before `set_value` so the `Change` it fires diffs against
        // itself (a no-op) instead of the previously-open document's text.
        self.last_text = text.clone();
        self.styles = styles;
        self.pending_style = None;
        self.input
            .update(cx, |input, cx| input.set_value(text, window, cx));
        self.render_decorations(cx);
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remarks = None;
        self.last_text.clear();
        self.styles.clear();
        self.pending_style = None;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
        self.decorations.clear(cx);
    }

    /// Reconcile `styles` with the editor's current text: find the common
    /// prefix/suffix against `last_text`, and carry the style bordering the
    /// edit onto whatever text replaced the middle.
    fn resync_styles(&mut self, cx: &mut Context<Self>) {
        let new_text = self.input.read(cx).value().to_string();
        if new_text == self.last_text {
            return;
        }
        let old_chars: Vec<char> = self.last_text.chars().collect();
        let new_chars: Vec<char> = new_text.chars().collect();

        let prefix = old_chars
            .iter()
            .zip(new_chars.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let old_rest = &old_chars[prefix..];
        let new_rest = &new_chars[prefix..];
        let suffix = old_rest
            .iter()
            .rev()
            .zip(new_rest.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();

        let old_removed_end = old_chars.len() - suffix;
        let new_inserted_end = new_chars.len() - suffix;
        // A pending style (set with no selection) overrides the usual
        // "inherit the neighboring char's style" rule for whatever text this
        // edit inserts — existing text is untouched either way.
        let inherited = if new_inserted_end > prefix {
            if let Some(pending) = &self.pending_style {
                pending.clone()
            } else if prefix > 0 {
                self.styles[prefix - 1].clone()
            } else {
                self.styles
                    .get(old_removed_end)
                    .cloned()
                    .unwrap_or_default()
            }
        } else {
            CharStyle::default()
        };

        let mut styles = Vec::with_capacity(new_chars.len());
        styles.extend_from_slice(&self.styles[..prefix]);
        styles.extend(std::iter::repeat_n(inherited, new_inserted_end - prefix));
        styles.extend_from_slice(&self.styles[old_removed_end..]);

        self.styles = styles;
        self.last_text = new_text;
        self.render_decorations(cx);
        self.push_to_model(cx);
    }

    /// Rebuild the (single) decoration collection from `styles`.
    fn render_decorations(&mut self, cx: &mut Context<Self>) {
        let text = self.input.read(cx).value().to_string();
        self.decorations
            .set(build_decorations(&text, &self.styles), cx);
    }

    fn push_to_model(&mut self, cx: &mut Context<Self>) {
        let Some(remarks) = self.remarks.clone() else {
            return;
        };
        let text = self.input.read(cx).value().to_string();
        let rich = to_rich_text(&text, &self.styles);
        remarks.update(cx, |remarks, cx| remarks.set_text(rich, cx));
    }

    /// The current selection as a `char`-index range, or `None` if the
    /// selection is empty (in which case formatting targets `pending_style`
    /// instead).
    fn selected_char_range(&self, cx: &Context<Self>) -> Option<Range<usize>> {
        let bytes = self.input.read(cx).selected_range();
        if bytes.start == bytes.end {
            return None;
        }
        let text = self.input.read(cx).value();
        Some(char_index_of_byte(&text, bytes.start)..char_index_of_byte(&text, bytes.end))
    }

    /// The style that formatting-with-no-selection should start from: the
    /// pending style if one is already active, else whatever the char just
    /// before the caret has (typing normally continues in that style).
    fn caret_style(&self, cx: &Context<Self>) -> CharStyle {
        if let Some(style) = &self.pending_style {
            return style.clone();
        }
        let bytes = self.input.read(cx).selected_range();
        let text = self.input.read(cx).value();
        let idx = char_index_of_byte(&text, bytes.start);
        self.styles
            .get(idx.saturating_sub(1))
            .cloned()
            .unwrap_or_default()
    }

    /// The style the toolbar buttons currently reflect. With a selection:
    /// whether *every* selected char already has each flag set — exactly the
    /// condition [`Self::toggle_flag`] itself checks to decide on vs. off, so
    /// a button's toggle state always matches what clicking it would do; the
    /// color is the first selected char's, mirroring
    /// [`Self::open_color_popover`]. With none: [`Self::caret_style`] (the
    /// pending style if one is set, else whatever the char before the caret
    /// has) — what typing next would carry.
    fn current_style(&self, cx: &Context<Self>) -> CharStyle {
        match self.selected_char_range(cx) {
            Some(range) => CharStyle {
                bold: range.clone().all(|i| self.styles[i].bold),
                italic: range.clone().all(|i| self.styles[i].italic),
                underline: range.clone().all(|i| self.styles[i].underline),
                color: self.styles.get(range.start).and_then(|s| s.color.clone()),
            },
            None => self.caret_style(cx),
        }
    }

    fn toggle_bold(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_flag(|s| &mut s.bold, window, cx);
    }
    fn toggle_italic(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_flag(|s| &mut s.italic, window, cx);
    }
    fn toggle_underline(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.toggle_flag(|s| &mut s.underline, window, cx);
    }

    /// With a selection: turn `flag` on for it unless every char already has
    /// it set, in which case turn it off (the usual mixed-selection toggle
    /// convention). With none: flip `flag` on `pending_style` instead, so it
    /// takes effect on whatever gets typed next without touching existing
    /// text (see `resync_styles`).
    fn toggle_flag(
        &mut self,
        flag: impl Fn(&mut CharStyle) -> &mut bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match self.selected_char_range(cx) {
            Some(range) => {
                let all_set = range.clone().all(|i| *flag(&mut self.styles[i]));
                for i in range {
                    *flag(&mut self.styles[i]) = !all_set;
                }
                self.pending_style = None;
                self.render_decorations(cx);
                self.push_to_model(cx);
            }
            None => {
                let mut style = self.caret_style(cx);
                let was_set = *flag(&mut style);
                *flag(&mut style) = !was_set;
                self.pending_style = Some(style);
            }
        }
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// With a target range: applies `hex` to it directly. Without one (the
    /// popover was opened with no selection): applies it to `pending_style`,
    /// so it takes effect starting with whatever gets typed next.
    fn apply_color(&mut self, hex: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        match self.color_target.clone() {
            Some(range) => {
                for i in range {
                    if let Some(style) = self.styles.get_mut(i) {
                        style.color = hex.clone();
                    }
                }
                self.render_decorations(cx);
                self.push_to_model(cx);
            }
            None => {
                let mut style = self.caret_style(cx);
                style.color = hex;
                self.pending_style = Some(style);
            }
        }
        self.input.update(cx, |s, cx| s.focus(window, cx));
    }

    /// Opens the color popover: with a selection, targets it directly and
    /// primes the picker with its current color; with none, targets
    /// `pending_style` (future typing) and primes the picker from the
    /// caret's style instead. Closing happens only via the popover panel's
    /// own `on_mouse_down_out` (see `color_panel`) — not here — since a
    /// click on the trigger while open would otherwise race the panel's
    /// close against this reopening it in the same event.
    fn open_color_popover(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let selection = self.selected_char_range(cx);
        let current = match &selection {
            Some(range) => self.styles.get(range.start).and_then(|s| s.color.clone()),
            None => self.caret_style(cx).color,
        };
        self.color_target = selection;
        self.color_open = true;
        self.color_picker.update(cx, |picker, cx| {
            match current.as_deref().and_then(parse_hex) {
                Some(color) => picker.set_value(color, window, cx),
                None => picker.clear_value(window, cx),
            }
        });
        cx.notify();
    }
}

/// The editor never shows fewer than this many lines …
const MIN_EDITOR_ROWS: usize = 4;
/// … nor grows taller than this many before it scrolls internally.
const MAX_EDITOR_ROWS: usize = 16;

impl Render for RemarksSection {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let enabled = self.remarks.is_some();

        // `EditorState` (code-editor mode) has no auto-grow of its own — it
        // always requests `flex_grow: 1, height: 100%` of its parent, with a
        // fixed one-line minimum (see `TextElement::request_layout`). Growing
        // with content is therefore the wrapper's job: size it from the
        // current line count, clamped to a sensible min/max, and let the
        // editor fill whatever that resolves to (scrolling internally past
        // the cap).
        //
        // `window.line_height()` alone reports the *ambient* text style's line
        // height — whatever the outer view last pushed — not the 13px size
        // the editor content below actually renders at (that only takes
        // effect once `.text_size(px(13.))`'s own prepaint runs). Using the
        // ambient value here overestimated every row by a constant amount,
        // which compounded into a growing dead zone at the bottom as more
        // lines were added. Force the same font size before asking.
        let mut editor_text_style = window.text_style();
        editor_text_style.font_size = AbsoluteLength::Pixels(px(13.));
        let line_height = editor_text_style.line_height_in_pixels(window.rem_size());

        let rows = self
            .input
            .read(cx)
            .value()
            .matches('\n')
            .count()
            .saturating_add(1)
            .clamp(MIN_EDITOR_ROWS, MAX_EDITOR_ROWS);

        // Must accomodate for row padding beteween lines, therefore + px(3.) per row
        let editor_height = line_height * rows as f32 + px(16.);

        let current = self.current_style(cx);

        let style_button = |glyph: gpui::SharedString,
                            id: &'static str,
                            active: bool,
                            on_click: fn(&mut Self, &mut Window, &mut Context<Self>),
                            map: fn(gpui::Stateful<gpui::Div>) -> gpui::Stateful<gpui::Div>,
                            cx: &mut Context<Self>| {
            div()
                .id(id)
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(radius)
                .border_1()
                .border_color(if active { c.primary } else { c.border })
                .bg(if active { c.accent_soft } else { c.surface })
                .text_size(px(13.))
                .text_color(if active { c.primary } else { c.foreground })
                .when(!enabled, |el| el.opacity(0.45))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(c.accent_soft))
                        .on_click(
                            cx.listener(move |this, _, window, cx| on_click(this, window, cx)),
                        )
                })
                .map(map)
                .child(glyph)
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(20.))
            .border_t_1()
            .border_color(c.border)
            .child(super::field_label(cx.t("detail.remarks.label"), &c))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(style_button(
                        cx.t("detail.remarks.bold-glyph"),
                        "fmt-bold",
                        current.bold,
                        Self::toggle_bold,
                        |el| el.font_weight(FontWeight::BOLD),
                        cx,
                    ))
                    .child(style_button(
                        cx.t("detail.remarks.italic-glyph"),
                        "fmt-italic",
                        current.italic,
                        Self::toggle_italic,
                        |el| el.italic(),
                        cx,
                    ))
                    .child(style_button(
                        cx.t("detail.remarks.underline-glyph"),
                        "fmt-underline",
                        current.underline,
                        Self::toggle_underline,
                        |el| el.underline(),
                        cx,
                    ))
                    .child(self.color_button(&c, radius, enabled, current.color.as_deref(), cx)),
            )
            .child(
                div()
                    .id("remarks-editor-scroll")
                    .h(editor_height)
                    .overflow_y_scroll()
                    .rounded(radius)
                    .border_1()
                    .border_color(c.border)
                    .bg(c.surface)
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(13.))
                    .child(Editor::new(&self.input)),
            )
    }
}

impl RemarksSection {
    /// The color-picker trigger + its floating popover, following the app's
    /// canvas-probe + `deferred(anchored())` popover convention (see
    /// `detail/judging_table.rs::discipline_selector`).
    fn color_button(
        &self,
        c: &crate::theme::PaletteColors,
        radius: Pixels,
        enabled: bool,
        current_color: Option<&str>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let open = self.color_open;
        let capture = self.color_bounds.clone();
        // The actual current colour — parsed from the selection's (or the
        // caret's pending) style, falling back to the foreground the text
        // would otherwise render in, same as the popover's own preview
        // swatch (`color_panel`'s `displayed.unwrap_or(c.foreground)`).
        let swatch_color = current_color.and_then(parse_hex).unwrap_or(c.foreground);

        let popover = open.then(|| self.color_bounds.get()).flatten().map(|b| {
            let anchor = point(b.origin.x, b.origin.y + b.size.height + px(4.));
            deferred(
                anchored()
                    .position(anchor)
                    .snap_to_window()
                    .child(self.color_panel(c, radius, cx)),
            )
            .with_priority(gpui_base::POPUP_PRIORITY)
        });

        div()
            .id("remarks-color")
            .relative()
            .flex()
            .flex_col()
            .child(
                canvas(
                    move |bounds, window, _cx| {
                        if capture.get() != Some(bounds) {
                            capture.set(Some(bounds));
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .id("remarks-color-trigger")
                    .flex()
                    .flex_none()
                    .items_center()
                    .justify_center()
                    .size(px(26.))
                    .rounded(radius)
                    .border_1()
                    .border_color(if current_color.is_some() {
                        c.primary
                    } else {
                        c.border
                    })
                    .bg(if current_color.is_some() {
                        c.accent_soft
                    } else {
                        c.surface
                    })
                    .when(!enabled, |el| el.opacity(0.45))
                    .when(enabled, |el| {
                        el.cursor_pointer()
                            .hover(|el| el.bg(c.accent_soft))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(|this, _, window, cx| {
                                    this.open_color_popover(window, cx)
                                }),
                            )
                    })
                    .child(
                        div()
                            .size(px(14.))
                            .rounded(px(3.))
                            .border_1()
                            .border_color(c.border)
                            .bg(swatch_color),
                    ),
            )
            .children(popover)
    }

    fn color_panel(
        &self,
        c: &crate::theme::PaletteColors,
        radius: Pixels,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let picker = self.color_picker.clone();
        let picker_read = self.color_picker.read(cx);
        let sliders = picker_read.sliders().clone();
        let hue = sliders.hue().read(cx).value().start();
        let sat = sliders.saturation().read(cx).value().start();
        let displayed = picker_read.displayed_color();
        let hex_input = picker_read.hex_input().clone();

        let swatches =
            div()
                .flex()
                .flex_wrap()
                .gap(px(6.))
                .children(PRESETS.into_iter().enumerate().map(|(index, value)| {
                    let color: Hsla = rgb_u32(value);
                    let click_picker = picker.clone();
                    ColorSwatch::new(("remarks-swatch", index), color)
                        .selected(displayed == Some(color))
                        .size(px(20.))
                        .rounded(px(4.))
                        .bg(color)
                        .border_1()
                        .border_color(c.border)
                        .on_click(move |color, _, window, cx| {
                            click_picker.update(cx, |p, cx| p.select_color(color, window, cx));
                        })
                }));

        let reset = {
            div()
                .id("remarks-color-reset")
                .cursor_pointer()
                .text_size(px(11.5))
                .text_color(c.muted_foreground)
                .hover(|el| el.text_color(c.foreground))
                .on_click(cx.listener(|this, _, window, cx| this.apply_color(None, window, cx)))
                .child(cx.t("detail.remarks.reset-color"))
        };

        div()
            .id("remarks-color-panel")
            .occlude()
            // On the panel itself, not the (much smaller) trigger — the
            // trigger's own hitbox never covers where this panel actually
            // paints (an anchored overlay), so attaching it there closed the
            // popover on every click inside it, including slider drags.
            .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                if this.color_open {
                    this.color_open = false;
                    cx.notify();
                }
            }))
            .w(px(210.))
            .p(px(10.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.surface)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.muted_foreground)
                            .child(cx.t("detail.remarks.text-color")),
                    )
                    .child(reset),
            )
            .child(swatches)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .flex_none()
                            .size(px(24.))
                            .rounded(px(5.))
                            .border_1()
                            .border_color(c.border)
                            .bg(displayed.unwrap_or(c.foreground)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Field::new("remarks-color-hex", &hex_input)),
                    ),
            )
            .child(hue_slider(sliders.hue(), cx))
            .child(saturation_slider(sliders.saturation(), hue, cx))
            .child(lightness_slider(sliders.lightness(), hue, sat, cx))
    }
}

fn rgb_u32(value: u32) -> Hsla {
    Rgba {
        r: ((value >> 16) & 0xff) as f32 / 255.,
        g: ((value >> 8) & 0xff) as f32 / 255.,
        b: (value & 0xff) as f32 / 255.,
        a: 1.,
    }
    .into()
}

const THUMB: f32 = 14.;

fn slider_thumb(state: &Entity<SliderState>, percentage: f32) -> impl IntoElement {
    SliderThumb::new(state)
        .absolute()
        .top(px(2.))
        .left(relative(percentage))
        .ml(px(-THUMB / 2.))
        .size(px(THUMB))
        .rounded_full()
        .bg(gpui::white())
        .border_2()
        .border_color(gpui::black().opacity(0.35))
        .shadow_sm()
}

fn hue_slider(state: &Entity<SliderState>, cx: &Context<RemarksSection>) -> impl IntoElement {
    const SEGMENTS: usize = 12;
    let percentage = state.read(cx).percentage().end;
    Slider::new(state).w_full().h(px(18.)).child(
        SliderTrack::new(state)
            .relative()
            .w_full()
            .h_full()
            .child(
                // `SliderIndicator` (not a plain `div`) — it's the element
                // whose `on_prepaint` records the bounds `update_value_by_position`
                // maps clicks/drags against; without one, `SliderState.bounds`
                // stays zero-sized forever and every position calc divides by
                // zero (dead clicks, a thumb parked at a NaN offset).
                SliderIndicator::new(state)
                    .absolute()
                    .top(px(5.))
                    .left_0()
                    .w_full()
                    .h(px(8.))
                    .flex()
                    .children((0..SEGMENTS).map(|i| {
                        let h0 = i as f32 / SEGMENTS as f32;
                        let h1 = (i + 1) as f32 / SEGMENTS as f32;
                        // `overflow_hidden` on the indicator doesn't clip its
                        // children to its own rounded corners, so each end
                        // segment rounds its own outer edge instead.
                        div()
                            .flex_1()
                            .h_full()
                            .when(i == 0, |el| el.rounded_l(px(4.)))
                            .when(i == SEGMENTS - 1, |el| el.rounded_r(px(4.)))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(hsla(h0, 1., 0.5, 1.), 0.),
                                linear_color_stop(hsla(h1, 1., 0.5, 1.), 1.),
                            ))
                    })),
            )
            .child(slider_thumb(state, percentage)),
    )
}

fn saturation_slider(
    state: &Entity<SliderState>,
    hue: f32,
    cx: &Context<RemarksSection>,
) -> impl IntoElement {
    let percentage = state.read(cx).percentage().end;
    Slider::new(state).w_full().h(px(18.)).child(
        SliderTrack::new(state)
            .relative()
            .w_full()
            .h_full()
            .child(
                SliderIndicator::new(state)
                    .absolute()
                    .top(px(5.))
                    .left_0()
                    .w_full()
                    .h(px(8.))
                    .rounded(px(4.))
                    .bg(linear_gradient(
                        90.,
                        linear_color_stop(hsla(hue, 0., 0.5, 1.), 0.),
                        linear_color_stop(hsla(hue, 1., 0.5, 1.), 1.),
                    )),
            )
            .child(slider_thumb(state, percentage)),
    )
}

fn lightness_slider(
    state: &Entity<SliderState>,
    hue: f32,
    sat: f32,
    cx: &Context<RemarksSection>,
) -> impl IntoElement {
    let percentage = state.read(cx).percentage().end;
    Slider::new(state).w_full().h(px(18.)).child(
        SliderTrack::new(state)
            .relative()
            .w_full()
            .h_full()
            .child(
                SliderIndicator::new(state)
                    .absolute()
                    .top(px(5.))
                    .left_0()
                    .w_full()
                    .h(px(8.))
                    .flex()
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .rounded_l(px(4.))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(hsla(0., 0., 0., 1.), 0.),
                                linear_color_stop(hsla(hue, sat, 0.5, 1.), 1.),
                            )),
                    )
                    .child(
                        div()
                            .flex_1()
                            .h_full()
                            .rounded_r(px(4.))
                            .bg(linear_gradient(
                                90.,
                                linear_color_stop(hsla(hue, sat, 0.5, 1.), 0.),
                                linear_color_stop(hsla(0., 0., 1., 1.), 1.),
                            )),
                    ),
            )
            .child(slider_thumb(state, percentage)),
    )
}

/// Formats a color as `#RRGGBB` (alpha is dropped — remark text has none).
fn hex_string(color: Hsla) -> String {
    let rgba = Rgba::from(color);
    let channel = |value: f32| (value.clamp(0., 1.) * 255.).round() as u32;
    format!(
        "#{:02X}{:02X}{:02X}",
        channel(rgba.r),
        channel(rgba.g),
        channel(rgba.b)
    )
}

fn char_index_of_byte(text: &str, byte: usize) -> usize {
    text[..byte].chars().count()
}

fn byte_of_char_boundaries(text: &str) -> Vec<usize> {
    text.char_indices()
        .map(|(b, _)| b)
        .chain(std::iter::once(text.len()))
        .collect()
}

/// Per-character style state — the authoritative formatting model.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct CharStyle {
    bold: bool,
    italic: bool,
    underline: bool,
    color: Option<String>,
}

impl CharStyle {
    fn to_highlight(&self) -> HighlightStyle {
        HighlightStyle {
            color: self.color.as_deref().and_then(parse_hex),
            font_weight: self.bold.then_some(FontWeight::BOLD),
            font_style: self.italic.then_some(FontStyle::Italic),
            // `UnderlineStyle::default()` has a zero `thickness` — invisible.
            underline: self.underline.then(|| UnderlineStyle {
                thickness: px(1.),
                ..Default::default()
            }),
            ..Default::default()
        }
    }
}

fn parse_hex(hex: &str) -> Option<Hsla> {
    let h = hex.strip_prefix('#').unwrap_or(hex);
    if h.len() != 6 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let component = |i: usize| {
        u8::from_str_radix(&h[i * 2..i * 2 + 2], 16)
            .ok()
            .map(|v| v as f32 / 255.)
    };
    Some(
        Rgba {
            r: component(0)?,
            g: component(1)?,
            b: component(2)?,
            a: 1.,
        }
        .into(),
    )
}

/// Coalesce `styles` into byte-ranged decorations, skipping default-styled
/// spans (nothing to render).
fn build_decorations(text: &str, styles: &[CharStyle]) -> Vec<TextDecoration> {
    let boundaries = byte_of_char_boundaries(text);
    let mut out = Vec::new();
    let mut i = 0;
    while i < styles.len() {
        let style = &styles[i];
        let mut j = i + 1;
        while j < styles.len() && styles[j] == *style {
            j += 1;
        }
        if *style != CharStyle::default() {
            out.push(TextDecoration::new(
                boundaries[i]..boundaries[j],
                style.to_highlight(),
            ));
        }
        i = j;
    }
    out
}

/// Flatten a [`RichText`] into plain text plus one [`CharStyle`] per char,
/// paragraphs joined by `\n` (the separator itself carries the default
/// style — it is never rendered).
fn flatten(rich: &RichText) -> (String, Vec<CharStyle>) {
    let mut text = String::new();
    let mut styles = Vec::new();
    for (i, para) in rich.paragraphs.iter().enumerate() {
        if i > 0 {
            text.push('\n');
            styles.push(CharStyle::default());
        }
        for run in &para.runs {
            for ch in run.text.chars() {
                text.push(ch);
                styles.push(CharStyle {
                    bold: run.bold,
                    italic: run.italic,
                    underline: run.underline,
                    color: run.color.clone(),
                });
            }
        }
    }
    (text, styles)
}

/// The inverse of [`flatten`]: split on `\n`, coalesce each line's chars into
/// runs.
fn to_rich_text(text: &str, styles: &[CharStyle]) -> RichText {
    let mut paragraphs = Vec::new();
    let mut idx = 0;
    for line in text.split('\n') {
        let n = line.chars().count();
        let line_styles = &styles[idx.min(styles.len())..(idx + n).min(styles.len())];
        paragraphs.push(Paragraph {
            runs: coalesce_runs(line, line_styles),
        });
        idx += n + 1;
    }
    RichText { paragraphs }
}

fn coalesce_runs(line: &str, styles: &[CharStyle]) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for (ch, style) in line.chars().zip(styles.iter()) {
        let same = out.last().is_some_and(|last: &Run| {
            last.bold == style.bold
                && last.italic == style.italic
                && last.underline == style.underline
                && last.color == style.color
        });
        if same {
            out.last_mut().unwrap().text.push(ch);
        } else {
            out.push(Run {
                text: ch.to_string(),
                bold: style.bold,
                italic: style.italic,
                underline: style.underline,
                color: style.color.clone(),
            });
        }
    }
    if out.is_empty() {
        out.push(Run::default());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn style(bold: bool, italic: bool, underline: bool, color: Option<&str>) -> CharStyle {
        CharStyle {
            bold,
            italic,
            underline,
            color: color.map(str::to_owned),
        }
    }

    #[test]
    fn flatten_round_trips_through_to_rich_text() {
        let rich = RichText {
            paragraphs: vec![
                Paragraph {
                    runs: vec![
                        Run {
                            text: "hallo ".into(),
                            ..Default::default()
                        },
                        Run {
                            text: "welt".into(),
                            bold: true,
                            color: Some("#FF0000".into()),
                            ..Default::default()
                        },
                    ],
                },
                Paragraph {
                    runs: vec![Run {
                        text: "zweite zeile".into(),
                        italic: true,
                        ..Default::default()
                    }],
                },
            ],
        };
        let (text, styles) = flatten(&rich);
        assert_eq!(text, "hallo welt\nzweite zeile");
        let back = to_rich_text(&text, &styles);
        assert_eq!(back, rich);
    }

    #[test]
    fn empty_line_round_trips() {
        let rich = RichText {
            paragraphs: vec![
                Paragraph {
                    runs: vec![Run::default()],
                },
                Paragraph {
                    runs: vec![Run::default()],
                },
            ],
        };
        let (text, styles) = flatten(&rich);
        assert_eq!(text, "\n");
        assert_eq!(to_rich_text(&text, &styles), rich);
    }

    #[test]
    fn build_decorations_skips_default_spans_and_coalesces() {
        let text = "a bold c";
        let styles = vec![
            style(false, false, false, None),
            style(false, false, false, None),
            style(true, false, false, None),
            style(true, false, false, None),
            style(true, false, false, None),
            style(true, false, false, None),
            style(false, false, false, None),
            style(false, false, false, None),
        ];
        let decorations = build_decorations(text, &styles);
        assert_eq!(decorations.len(), 1);
        assert_eq!(decorations[0].range, 2..6);
        assert_eq!(decorations[0].style.font_weight, Some(FontWeight::BOLD));
    }

    #[test]
    fn prefix_suffix_diff_inherits_surrounding_style() {
        // Simulates `resync_styles`'s core diff without needing a live editor.
        let old_text: Vec<char> = "bold text".chars().collect();
        let new_text: Vec<char> = "bold new text".chars().collect();
        let old_styles = vec![style(true, false, false, None); old_text.len()];

        let prefix = old_text
            .iter()
            .zip(new_text.iter())
            .take_while(|(a, b)| a == b)
            .count();
        let old_rest = &old_text[prefix..];
        let new_rest = &new_text[prefix..];
        let suffix = old_rest
            .iter()
            .rev()
            .zip(new_rest.iter().rev())
            .take_while(|(a, b)| a == b)
            .count();
        let old_removed_end = old_text.len() - suffix;
        let new_inserted_end = new_text.len() - suffix;
        let inherited = old_styles[prefix - 1].clone();

        let mut styles = Vec::new();
        styles.extend_from_slice(&old_styles[..prefix]);
        styles.extend(std::iter::repeat_n(inherited, new_inserted_end - prefix));
        styles.extend_from_slice(&old_styles[old_removed_end..]);

        assert_eq!(styles.len(), new_text.len());
        assert!(styles.iter().all(|s| s.bold));
    }
}
