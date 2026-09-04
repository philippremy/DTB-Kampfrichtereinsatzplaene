//! The trailing free-form remarks section.
//!
//! A plain multi-line field plus a small **F / K / U** formatting toolbar.
//! There is no inline WYSIWYG (gpui-base only styles ranges in its code-editor
//! mode) — instead the toolbar wraps the current selection in lightweight
//! markdown-style markers (`**bold**`, `*kursiv*`, `__unterstrichen__`) which
//! [`parse`] turns into styled [`Run`]s on every change. The markers stay
//! visible while editing; export / preview render the real styling from the
//! runs.

use std::ops::Range;

use gpui::{
    AppContext, Context, Entity, FontWeight, InteractiveElement, IntoElement, ParentElement,
    Render, StatefulInteractiveElement, Styled, Subscription, Window, div, prelude::FluentBuilder,
    px,
};
use gpui_base::input::{InputEvent, Textarea, TextareaState};

use crate::model::{Paragraph, RichText, Run};
use crate::store::RemarksEditor;
use crate::theme::ActiveTheme;

pub struct RemarksSection {
    remarks: Option<Entity<RemarksEditor>>,
    input: Entity<TextareaState>,
    _sub: Subscription,
}

impl RemarksSection {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("Anmerkungen für das Dokumentende …")
                .auto_grow(3, 12)
        });
        let sub = cx.subscribe(&input, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) {
                this.sync(cx);
            }
        });
        Self {
            remarks: None,
            input,
            _sub: sub,
        }
    }

    pub fn rebind(
        &mut self,
        remarks: Entity<RemarksEditor>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let markers = serialize(remarks.read(cx).text());
        self.remarks = Some(remarks);
        self.input
            .update(cx, |input, cx| input.set_value(markers, window, cx));
    }

    pub fn clear(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.remarks = None;
        self.input
            .update(cx, |input, cx| input.set_value("", window, cx));
    }

    /// Reparse the textarea's marker text into the model.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let Some(remarks) = self.remarks.clone() else {
            return;
        };
        let text = parse(&self.input.read(cx).value());
        remarks.update(cx, |remarks, cx| remarks.set_text(text, cx));
    }

    /// Toggle `marker` around the current selection: unwrap if the selection is
    /// already wrapped (markers just inside or just outside it), otherwise wrap.
    fn apply_marker(&mut self, marker: Marker, window: &mut Window, cx: &mut Context<Self>) {
        if self.remarks.is_none() {
            return;
        }
        let tok = marker.token();
        let full = self.input.read(cx).value().to_string();
        let Range { start, end } = self.input.read(cx).selected_range();
        let inner = &full[start..end];

        let (value, sel) =
            if inner.len() >= 2 * tok.len() && inner.starts_with(tok) && inner.ends_with(tok) {
                // `**word**` selected → strip the markers inside the selection.
                let stripped = &inner[tok.len()..inner.len() - tok.len()];
                (
                    format!("{}{}{}", &full[..start], stripped, &full[end..]),
                    start..end - 2 * tok.len(),
                )
            } else if full[..start].ends_with(tok) && full[end..].starts_with(tok) {
                // `**` `word` `**` with the markers just outside the selection.
                (
                    format!(
                        "{}{}{}",
                        &full[..start - tok.len()],
                        inner,
                        &full[end + tok.len()..]
                    ),
                    start - tok.len()..end - tok.len(),
                )
            } else {
                // Wrap the selection (or, if empty, drop an empty marker pair and
                // park the cursor between the two halves).
                (
                    format!("{}{tok}{inner}{tok}{}", &full[..start], &full[end..]),
                    start + tok.len()..end + tok.len(),
                )
            };

        self.input.update(cx, |state, cx| {
            state.replace_all(value, window, cx);
            state.set_selected_range(sel, cx);
            state.focus(window, cx);
        });
        self.sync(cx);
    }
}

impl Render for RemarksSection {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let enabled = self.remarks.is_some();

        let button = |marker: Marker, cx: &mut Context<Self>| {
            div()
                .id(marker.id())
                .flex()
                .flex_none()
                .items_center()
                .justify_center()
                .size(px(26.))
                .rounded(radius)
                .border_1()
                .border_color(c.border)
                .bg(c.surface)
                .text_size(px(13.))
                .text_color(c.foreground)
                .when(!enabled, |el| el.opacity(0.45))
                .when(enabled, |el| {
                    el.cursor_pointer()
                        .hover(|el| el.bg(c.accent_soft))
                        .on_click(cx.listener(move |this, _, window, cx| {
                            this.apply_marker(marker, window, cx)
                        }))
                })
                .map(|el| match marker {
                    Marker::Bold => el.font_weight(FontWeight::BOLD),
                    Marker::Italic => el.italic(),
                    Marker::Underline => el.underline(),
                })
                .child(marker.glyph())
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .p(px(20.))
            .border_t_1()
            .border_color(c.border)
            .child(super::field_label("Anmerkungen", &c))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .child(button(Marker::Bold, cx))
                    .child(button(Marker::Italic, cx))
                    .child(button(Marker::Underline, cx)),
            )
            .child(
                div()
                    .rounded(radius)
                    .border_1()
                    .border_color(c.border)
                    .bg(c.surface)
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(13.))
                    .child(Textarea::new(&self.input)),
            )
    }
}

/// One of the three supported inline styles.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Marker {
    Bold,
    Italic,
    Underline,
}

impl Marker {
    /// The marker token. `**` is checked before `*` when parsing.
    fn token(self) -> &'static str {
        match self {
            Marker::Bold => "**",
            Marker::Italic => "*",
            Marker::Underline => "__",
        }
    }

    fn id(self) -> &'static str {
        match self {
            Marker::Bold => "fmt-bold",
            Marker::Italic => "fmt-italic",
            Marker::Underline => "fmt-underline",
        }
    }

    /// German word-processor convention: Fett / Kursiv / Unterstrichen.
    fn glyph(self) -> &'static str {
        match self {
            Marker::Bold => "F",
            Marker::Italic => "K",
            Marker::Underline => "U",
        }
    }
}

/// Per-run style state while scanning a line.
#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Style {
    bold: bool,
    italic: bool,
    underline: bool,
}

impl Style {
    fn of(run: &Run) -> Self {
        Self {
            bold: run.bold,
            italic: run.italic,
            underline: run.underline,
        }
    }

    fn toggled(self, marker: Marker) -> Self {
        match marker {
            Marker::Bold => Self {
                bold: !self.bold,
                ..self
            },
            Marker::Italic => Self {
                italic: !self.italic,
                ..self
            },
            Marker::Underline => Self {
                underline: !self.underline,
                ..self
            },
        }
    }

    fn run(self, text: String) -> Run {
        Run {
            text,
            bold: self.bold,
            italic: self.italic,
            underline: self.underline,
        }
    }
}

/// Parse marker text into styled runs, one paragraph per line.
///
/// A stateful scan: each `**` / `__` / `*` toggles its style for the rest of
/// the line (or until the matching marker). Unbalanced markers simply stay in
/// effect to the end of the line. There is no escaping — a literal asterisk in
/// this field will read as a marker.
fn parse(text: &str) -> RichText {
    RichText {
        paragraphs: text
            .split('\n')
            .map(|line| Paragraph {
                runs: coalesce(parse_line(line)),
            })
            .collect(),
    }
}

fn parse_line(line: &str) -> Vec<Run> {
    let mut runs = Vec::new();
    let mut style = Style::default();
    let mut buf = String::new();
    let mut rest = line;

    while !rest.is_empty() {
        let marker = if rest.starts_with("**") {
            Some(Marker::Bold)
        } else if rest.starts_with("__") {
            Some(Marker::Underline)
        } else if rest.starts_with('*') {
            Some(Marker::Italic)
        } else {
            None
        };

        if let Some(marker) = marker {
            if !buf.is_empty() {
                runs.push(style.run(std::mem::take(&mut buf)));
            }
            style = style.toggled(marker);
            rest = &rest[marker.token().len()..];
        } else {
            let ch = rest.chars().next().unwrap();
            buf.push(ch);
            rest = &rest[ch.len_utf8()..];
        }
    }

    if !buf.is_empty() {
        runs.push(style.run(buf));
    }
    runs
}

/// Merge adjacent runs that share a style; guarantee at least one run so an
/// empty line round-trips.
fn coalesce(runs: Vec<Run>) -> Vec<Run> {
    let mut out: Vec<Run> = Vec::new();
    for run in runs {
        if run.text.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(last) if Style::of(last) == Style::of(&run) => last.text.push_str(&run.text),
            _ => out.push(run),
        }
    }
    if out.is_empty() {
        out.push(Run::default());
    }
    out
}

/// Render styled runs back to marker text — the inverse of [`parse`].
fn serialize(text: &RichText) -> String {
    text.paragraphs
        .iter()
        .map(|paragraph| serialize_line(&coalesce(paragraph.runs.clone())))
        .collect::<Vec<_>>()
        .join("\n")
}

fn serialize_line(runs: &[Run]) -> String {
    let mut out = String::new();
    let mut style = Style::default();
    for run in runs {
        push_transition(&mut out, style, Style::of(run));
        out.push_str(&run.text);
        style = Style::of(run);
    }
    push_transition(&mut out, style, Style::default());
    out
}

/// Emit the markers that take `from` to `to`. Order is Bold, Underline, Italic
/// so a combined open/close reads as `**` + `*` = `***`, which [`parse_line`]
/// splits the same way (it checks `**` before `*`).
fn push_transition(out: &mut String, from: Style, to: Style) {
    if from.bold != to.bold {
        out.push_str("**");
    }
    if from.underline != to.underline {
        out.push_str("__");
    }
    if from.italic != to.italic {
        out.push('*');
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runs(line: &str) -> Vec<(String, bool, bool, bool)> {
        parse_line(line)
            .into_iter()
            .map(|r| (r.text, r.bold, r.italic, r.underline))
            .collect()
    }

    #[test]
    fn plain_text_is_one_run() {
        assert_eq!(
            runs("hallo welt"),
            vec![("hallo welt".into(), false, false, false)]
        );
    }

    #[test]
    fn bold_italic_underline_spans() {
        assert_eq!(
            runs("a **b** c *d* e __f__"),
            vec![
                ("a ".into(), false, false, false),
                ("b".into(), true, false, false),
                (" c ".into(), false, false, false),
                ("d".into(), false, true, false),
                (" e ".into(), false, false, false),
                ("f".into(), false, false, true),
            ]
        );
    }

    #[test]
    fn combined_styles_nest() {
        assert_eq!(
            runs("***fett kursiv***"),
            vec![("fett kursiv".into(), true, true, false)]
        );
    }

    #[test]
    fn round_trips_through_serialize() {
        for src in [
            "nur text",
            "a **b** c",
            "***beides*** und __unterstrichen__",
            "leer\n\nund weiter",
            "**fett** dann *kursiv*",
        ] {
            let there = parse(src);
            let back = serialize(&there);
            assert_eq!(parse(&back), there, "round-trip for {src:?}");
        }
    }
}
