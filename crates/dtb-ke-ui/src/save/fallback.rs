//! The cross-platform fallback save dialog: a small gpui window that collects
//! the format + options, then hands the filename to gpui's plain path picker.
//! Used on Linux (no portable native customisable save panel) and whenever
//! `DTB_KE_SAVE_FALLBACK` is set.

use dtb_ke_export::{PdfExport, PdfStandard, pdf_standard_conflicts};
use futures::channel::oneshot;
use gpui::{
    App, AppContext, Bounds, Context, FocusHandle, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Render, Size, StatefulInteractiveElement, Styled, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px,
};

use crate::components::button::{Button, ButtonTone};
use crate::components::icon::Icon;
use crate::components::segmented::Segmented;
use crate::save::{ExportFormat, FormatKind, SaveChoice, with_extension};
use crate::theme::ActiveTheme;

fn field_label(text: &str, c: Hsla) -> gpui::Div {
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c)
        .child(text.to_uppercase())
}

/// A checkbox row (visual only; the caller wraps it in a clickable element).
fn checkbox_row(
    label: impl Into<gpui::SharedString>,
    checked: bool,
    c: Hsla,
    on: Hsla,
) -> gpui::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(3.))
        .child(
            div()
                .flex_none()
                .size(px(16.))
                .rounded(px(3.))
                .border_1()
                .border_color(if checked { on } else { c })
                .flex()
                .items_center()
                .justify_center()
                .when(checked, |el| el.bg(on))
                .when(checked, |el| {
                    el.child(Icon::Check.size(px(11.)).color(gpui::white()))
                }),
        )
        .child(div().text_size(px(12.5)).child(label.into()))
}

pub(super) fn prompt(default_name: String, cx: &mut App) -> oneshot::Receiver<Option<SaveChoice>> {
    let (tx, rx) = oneshot::channel();

    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(460.), px(480.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("Exportieren".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        is_resizable: false,
        ..Default::default()
    };

    if cx
        .open_window(options, |window, cx| {
            cx.new(|cx| SaveOptions::new(default_name, tx, window, cx))
        })
        .is_err()
    {
        log::error!("save dialog: could not open the in-app export window");
    }

    rx
}

struct SaveOptions {
    name: String,
    format: ExportFormat,
    /// Set once the path picker is up, so the window renders "waiting".
    picking: bool,
    tx: Option<oneshot::Sender<Option<SaveChoice>>>,
    focus: FocusHandle,
}

impl SaveOptions {
    fn new(
        name: String,
        tx: oneshot::Sender<Option<SaveChoice>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        Self {
            name,
            format: ExportFormat::default_choice(),
            picking: false,
            tx: Some(tx),
            focus: cx.focus_handle(),
        }
    }

    fn set_kind(&mut self, kind: FormatKind, cx: &mut Context<Self>) {
        self.format = kind.with_options(&self.format);
        cx.notify();
    }

    fn toggle_standard(&mut self, standard: PdfStandard, cx: &mut Context<Self>) {
        if let ExportFormat::Pdf(PdfExport { standards }) = &mut self.format {
            if let Some(i) = standards.iter().position(|s| *s == standard) {
                standards.remove(i);
            } else {
                standards.push(standard);
            }
            cx.notify();
        }
    }

    fn toggle_embed_fonts(&mut self, cx: &mut Context<Self>) {
        if let ExportFormat::Docx(o) = &mut self.format {
            o.embed_fonts = !o.embed_fonts;
            cx.notify();
        }
    }

    fn standards_error(&self) -> Option<String> {
        match &self.format {
            ExportFormat::Pdf(o) if !o.standards.is_empty() => {
                dtb_ke_export::validate_pdf_standards(&o.standards).err()
            }
            _ => None,
        }
    }

    fn cancel(&mut self, cx: &mut Context<Self>) {
        if let Some(tx) = self.tx.take() {
            let _ = tx.send(None);
        }
        cx.notify();
        // The window closes itself via `on_release` on the caller side; here we
        // just drop our handle to the sender.
    }

    fn choose_path(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.standards_error().is_some() {
            return;
        }
        self.picking = true;
        cx.notify();

        let suggested = format!("{}.{}", self.name.trim(), self.format.extension());
        let dir = dirs::document_dir()
            .or_else(dirs::home_dir)
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let receiver = cx.prompt_for_new_path(&dir, Some(&suggested));
        let format = self.format.clone();

        cx.spawn_in(window, async move |this, cx| {
            let picked = receiver.await.ok().and_then(|r| r.ok()).flatten();
            this.update_in(cx, |this, window, cx| {
                this.picking = false;
                match picked {
                    Some(path) => {
                        if let Some(tx) = this.tx.take() {
                            let _ = tx.send(Some(SaveChoice {
                                path: with_extension(path, &format),
                                format,
                            }));
                        }
                        window.remove_window();
                    }
                    None => cx.notify(),
                }
            })
            .ok();
        })
        .detach();
    }
}

impl Render for SaveOptions {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let kind = self.format.kind();
        let selected = FormatKind::ALL.iter().position(|k| *k == kind).unwrap_or(0);
        let error = self.standards_error();
        let lead = crate::skin::titlebar::content_leading_inset(window);

        div()
            .track_focus(&self.focus)
            .bg(c.background)
            .text_color(c.foreground)
            .flex()
            .flex_col()
            .child(
                // Title strip — mirrors the preview window's; leaves room for
                // the traffic lights on macOS.
                div()
                    .flex_none()
                    .h(px(34.))
                    .pl(lead)
                    .pr(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(c.border)
                    .bg(c.chrome)
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child("Exportieren"),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .gap(px(12.))
                    .p(px(20.))
                    .child(
                        div()
                            .text_size(px(15.))
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child(format!("„{}“ exportieren", self.name.trim())),
                    )
                    .child(field_label("Format", c.muted_foreground))
                    .child(
                        Segmented::new(
                            "export-format",
                            FormatKind::ALL.map(|k| match k {
                                FormatKind::Pdf => "PDF",
                                FormatKind::Docx => "DOCX",
                                FormatKind::Blob => "Rohdaten",
                            }),
                            selected,
                        )
                        .on_select({
                            let this = cx.entity().downgrade();
                            move |idx, _window, cx| {
                                this.update(cx, |this, cx| this.set_kind(FormatKind::ALL[idx], cx))
                                    .ok();
                            }
                        }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(2.))
                            .when(kind == FormatKind::Pdf, |el| {
                                let conflicts = pdf_standard_conflicts(self.format.pdf_standards());
                                el.child(field_label("Standards", c.muted_foreground))
                                    .children(PdfStandard::ALL.map(|s| {
                                        let checked = self.format.pdf_standards().contains(&s);
                                        let blocked = conflicts
                                            .iter()
                                            .find(|(c, _)| *c == s)
                                            .map(|(_, r)| r.clone());
                                        let row = div()
                                            .id(("std", s as usize))
                                            .child(checkbox_row(
                                                s.label(),
                                                checked,
                                                c.line_strong,
                                                if blocked.is_some() {
                                                    c.line_strong
                                                } else {
                                                    c.primary
                                                },
                                            ))
                                            .children(blocked.clone().map(|r| {
                                                div()
                                                    .ml(px(24.))
                                                    .text_size(px(11.))
                                                    .text_color(c.muted_foreground)
                                                    .child(r)
                                            }));
                                        if blocked.is_some() {
                                            row.opacity(0.55)
                                        } else {
                                            row.cursor_pointer().on_click(cx.listener(
                                                move |this, _, _w, cx| this.toggle_standard(s, cx),
                                            ))
                                        }
                                    }))
                                    .children(error.clone().map(|e| {
                                        div()
                                            .mt(px(6.))
                                            .text_size(px(11.5))
                                            .text_color(c.critical)
                                            .child(e)
                                    }))
                            })
                            .when(kind == FormatKind::Docx, |el| {
                                el.child(
                                    div()
                                        .id("embed-fonts")
                                        .cursor_pointer()
                                        .on_click(
                                            cx.listener(|this, _, _w, cx| this.toggle_embed_fonts(cx)),
                                        )
                                        .child(checkbox_row(
                                            "Schriften einbetten",
                                            self.format.docx_embed_fonts(),
                                            c.line_strong,
                                            c.primary,
                                        )),
                                )
                                .child(
                                    div()
                                        .mt(px(6.))
                                        .text_size(px(11.5))
                                        .text_color(c.muted_foreground)
                                        .child(
                                            "Schriften einbetten macht die Datei eigenständig \
                                            (~600 KB größer).",
                                        ),
                                )
                            })
                            .when(kind == FormatKind::Blob, |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(c.muted_foreground)
                                        .child(
                                            "Kopie der Rohdaten zum Sichern oder Weitergeben. \
                                            Kann über „Wettkampf importieren“ wieder eingelesen werden.",
                                        ),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .justify_end()
                            .gap(px(8.))
                            .rounded(radius)
                            .child(
                                Button::new("cancel", "Abbrechen")
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel(cx);
                                        window.remove_window();
                                    })),
                            )
                            .child(
                                Button::new("save", "Speichern unter …")
                                    .tone(ButtonTone::Primary)
                                    .disabled(self.picking || error.is_some())
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.choose_path(window, cx)),
                                    ),
                            ),
                    )
            )
    }
}
