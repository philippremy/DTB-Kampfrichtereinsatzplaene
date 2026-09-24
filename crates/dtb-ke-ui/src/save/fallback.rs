//! The cross-platform fallback save dialog: a small gpui window that collects
//! the format + options, then hands the filename to gpui's plain path picker.
//! Used on Linux (no portable native customisable save panel) and whenever
//! `DTB_KE_SAVE_FALLBACK` is set.

use dtb_ke_export::{PdfExport, PdfStandard, pdf_standard_conflicts};
use futures::channel::oneshot;
use gpui_kit::{
    App, AppContext, Bounds, Context, FocusHandle, FontWeight, Hsla, InteractiveElement,
    IntoElement, ParentElement, Pixels, Render, Size, StatefulInteractiveElement, Styled,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, prelude::FluentBuilder, px,
};

use crate::components::button::{Button, ButtonTone};
use crate::components::icon::Icon;
use crate::components::segmented::Segmented;
use crate::i18n::ActiveLocale;
use crate::save::{
    ExportFormat, FormatKind, SaveChoice, Target, pdf_standard_conflict_message,
    pdf_standard_label, with_extension,
};
use crate::theme::ActiveTheme;

fn field_label(text: impl Into<gpui_kit::SharedString>, c: Hsla) -> gpui_kit::Div {
    let text = text.into();
    div()
        .text_size(px(10.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c)
        .child(text.to_uppercase())
}

/// A checkbox row (visual only; the caller wraps it in a clickable element).
fn checkbox_row(
    label: impl Into<gpui_kit::SharedString>,
    checked: bool,
    c: Hsla,
    on: Hsla,
    radius: Pixels,
) -> gpui_kit::Div {
    div()
        .flex()
        .items_center()
        .gap(px(8.))
        .py(px(3.))
        .child(
            div()
                .flex_none()
                .size(px(16.))
                .rounded(radius)
                .border_1()
                .border_color(if checked { on } else { c })
                .flex()
                .items_center()
                .justify_center()
                .when(checked, |el| el.bg(on))
                .when(checked, |el| {
                    el.child(Icon::Check.size(px(11.)).color(gpui_kit::white()))
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
            title: Some(cx.t("save.window-title")),
            appears_transparent: true,
            ..Default::default()
        }),
        is_resizable: false,
        ..Default::default()
    };

    if crate::sheet::enabled() {
        crate::sheet::present(cx, "export", &options, move |window, cx| {
            new_panel(default_name, tx, window, cx)
        });
        return rx;
    }
    if cx
        .open_window(options, |window, cx| new_panel(default_name, tx, window, cx))
        .is_err()
    {
        log::error!("save dialog: could not open the in-app export window");
    }

    rx
}

fn new_panel(
    name: String,
    tx: oneshot::Sender<Option<SaveChoice>>,
    window: &mut Window,
    cx: &mut App,
) -> gpui_kit::Entity<SaveOptions> {
    let panel = cx.new(|cx| SaveOptions::new(name, tx, window, cx));
    // Test aid: `DTB_KE_SAVE_AUTOCONFIRM=pdf|docx|blob` picks that format and confirms the panel by
    // itself a second later, so the export pipeline can be driven without touching the UI.
    if let Ok(kind) = std::env::var("DTB_KE_SAVE_AUTOCONFIRM") {
        panel.update(cx, |panel, cx| {
            let kind = match kind.as_str() {
                "docx" => FormatKind::Docx,
                "blob" => FormatKind::Blob,
                _ => FormatKind::Pdf,
            };
            panel.set_kind(kind, cx);
            cx.spawn_in(window, async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                this.update_in(cx, |this, window, cx| this.choose_path(window, cx)).ok();
            })
            .detach();
        });
    }
    panel
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
        if crate::sheet::enabled() {
            // iPadOS has no save panel to ask: hand back a deferred target. The exporter stages
            // the file in the app's container and shows the document picker once it exists.
            let file_name = with_extension(std::path::PathBuf::from(self.name.trim()), &self.format)
                .to_string_lossy()
                .into_owned();
            if let Some(tx) = self.tx.take() {
                let choice = SaveChoice {
                    target: Target::Deferred { file_name },
                    format: self.format.clone(),
                };
                if tx.send(Some(choice)).is_err() {
                    log::debug!("save dialog: the receiver went away before the choice was made");
                }
            }
            crate::sheet::close(window, cx);
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
                                target: Target::Path(with_extension(path, &format)),
                                format,
                            }));
                        }
                        crate::sheet::close(window, cx);
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
        let checkbox_radius = cx.theme().skin.radius_control_px().min(px(4.));
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
            .when(!crate::sheet::enabled(), |el| el.child(
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
                    .child(cx.t("save.window-title")),
            ))
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
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(cx.t_fmt("save.export-title", &[("name", self.name.trim())])),
                    )
                    .child(field_label(cx.t("save.format-label"), c.muted_foreground))
                    .child({
                        let locale = cx.global::<crate::i18n::Locale>().clone();
                        Segmented::new(
                            "export-format",
                            FormatKind::ALL.map(|k| k.short_label(&locale)),
                            selected,
                        )
                        .on_select({
                            let this = cx.entity().downgrade();
                            move |idx, _window, cx| {
                                this.update(cx, |this, cx| this.set_kind(FormatKind::ALL[idx], cx))
                                    .ok();
                            }
                        })
                    })
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_h(px(0.))
                            .gap(px(2.))
                            .when(kind == FormatKind::Pdf, |el| {
                                let conflicts = pdf_standard_conflicts(self.format.pdf_standards());
                                let locale = cx.global::<crate::i18n::Locale>().clone();
                                el.child(field_label(
                                    cx.t("save.standards-label"),
                                    c.muted_foreground,
                                ))
                                .children(PdfStandard::ALL.map(|s| {
                                    let checked = self.format.pdf_standards().contains(&s);
                                    let blocked =
                                        conflicts.iter().find(|(c, _)| *c == s).map(|(_, r)| {
                                            pdf_standard_conflict_message(s, *r, &locale)
                                        });
                                    let row = div()
                                        .id(("std", s as usize))
                                        .child(checkbox_row(
                                            pdf_standard_label(s, &locale),
                                            checked,
                                            c.line_strong,
                                            if blocked.is_some() {
                                                c.line_strong
                                            } else {
                                                c.primary
                                            },
                                            checkbox_radius,
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
                                        .on_click(cx.listener(|this, _, _w, cx| {
                                            this.toggle_embed_fonts(cx)
                                        }))
                                        .child(checkbox_row(
                                            cx.t("save.embed-fonts-label"),
                                            self.format.docx_embed_fonts(),
                                            c.line_strong,
                                            c.primary,
                                            checkbox_radius,
                                        )),
                                )
                                .child(
                                    div()
                                        .mt(px(6.))
                                        .text_size(px(11.5))
                                        .text_color(c.muted_foreground)
                                        .child(cx.t("save.embed-fonts-description")),
                                )
                            })
                            .when(kind == FormatKind::Blob, |el| {
                                el.child(
                                    div()
                                        .text_size(px(12.))
                                        .text_color(c.muted_foreground)
                                        .child(cx.t("save.blob-fallback-description")),
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
                                Button::new("cancel", cx.t("save.cancel-button"))
                                    .tone(ButtonTone::Ghost)
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.cancel(cx);
                                        crate::sheet::close(window, cx);
                                    })),
                            )
                            .child(
                                Button::new("save", cx.t("save.save-button"))
                                    .tone(ButtonTone::Primary)
                                    .disabled(self.picking || error.is_some())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.choose_path(window, cx)
                                    })),
                            ),
                    ),
            )
    }
}
