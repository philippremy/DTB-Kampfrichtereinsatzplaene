//! The live Typst preview window.
//!
//! A separate window that re-renders the selected competition to page bitmaps
//! whenever it (or the selection) changes, debounced. The render runs on the
//! background executor via a shared [`Exporter`]; a [`gpui::ScrollHandle`] owned
//! by the view keeps the scroll position across re-renders so visual proofing
//! stays put.

use std::sync::Arc;
use std::time::Duration;

use dtb_ke_export::{Exporter, PreviewOptions};
use gpui::{
    App, Bounds, Context, Entity, InteractiveElement, IntoElement, ParentElement, RenderImage,
    ScrollHandle, Size, StatefulInteractiveElement, Styled, Subscription, Task, TitlebarOptions,
    Window, WindowBounds, WindowOptions, div, hsla, img, prelude::FluentBuilder, px, white,
};
use image::{Frame, RgbaImage};
use log::{debug, trace, warn};
use uuid::Uuid;

use crate::store::AppStore;
use crate::theme::ActiveTheme;

/// Quiet period after the last edit before a re-render fires.
const DEBOUNCE: Duration = Duration::from_millis(350);
/// Raster scale. 2.0 ≈ 300 dpi — sharp enough scaled down to page width.
const PIXELS_PER_POINT: f32 = 3.0;

pub struct PreviewWindow {
    store: Entity<AppStore>,
    exporter: Arc<Exporter>,

    /// Rendered pages (BGRA `RenderImage`s) and their natural pixel sizes.
    pages: Vec<Page>,
    status: Status,

    /// Bumped per render request; a finished render with a stale generation is
    /// dropped (a newer edit already superseded it).
    generation: u64,
    render_task: Option<Task<()>>,

    /// Owned by the view, so the scroll offset survives a full re-render.
    scroll: ScrollHandle,

    watched: Option<Uuid>,
    _store_sub: Subscription,
    _doc_sub: Option<Subscription>,
}

struct Page {
    image: Arc<RenderImage>,
    width: u32,
    height: u32,
}

enum Status {
    /// No competition selected.
    Idle,
    /// Rendering and nothing to show yet.
    Rendering,
    /// Re-rendering; the previous pages are still on screen.
    Refreshing,
    Ready,
    Failed(String),
}

impl PreviewWindow {
    pub fn new(store: Entity<AppStore>, exporter: Arc<Exporter>, cx: &mut Context<Self>) -> Self {
        let store_sub = cx.observe(&store, |this, _, cx| this.rewatch(cx));
        let mut this = Self {
            store,
            exporter,
            pages: Vec::new(),
            status: Status::Idle,
            generation: 0,
            render_task: None,
            scroll: ScrollHandle::new(),
            watched: None,
            _store_sub: store_sub,
            _doc_sub: None,
        };
        this.rewatch(cx);
        this
    }

    /// Keep `_doc_sub` pointed at the selected document, and (re)render.
    fn rewatch(&mut self, cx: &mut Context<Self>) {
        let selected = self.store.read(cx).selected_id();
        if selected != self.watched {
            self.watched = selected;
            let doc = self.store.read(cx).selected_document().cloned();
            self._doc_sub = doc.map(|doc| cx.observe(&doc, |this, _, cx| this.schedule_render(cx)));
        }
        self.schedule_render(cx);
    }

    fn schedule_render(&mut self, cx: &mut Context<Self>) {
        let Some(doc) = self.store.read(cx).selected_document().cloned() else {
            self.pages.clear();
            self.status = Status::Idle;
            self.render_task = None;
            cx.notify();
            return;
        };

        self.generation += 1;
        let generation = self.generation;
        self.status = if self.pages.is_empty() {
            Status::Rendering
        } else {
            Status::Refreshing
        };
        cx.notify();

        let exporter = self.exporter.clone();
        self.render_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(DEBOUNCE).await;

            // Snapshot *after* the debounce so we render the settled state.
            let Ok(dto) = this.update(cx, |_, cx| doc.read(cx).to_dto(cx)) else {
                return;
            };

            let result = cx
                .background_executor()
                .spawn(async move {
                    exporter
                        .render_previews(
                            &dto,
                            PreviewOptions {
                                pixels_per_point: PIXELS_PER_POINT,
                            },
                        )
                        .map(|pages| {
                            pages
                                .into_iter()
                                .map(|mut p| {
                                    // typst-render gives RGBA; gpui wants BGRA.
                                    dtb_ke_util::pixel::swap_rb(&mut p.rgba);
                                    (p.width, p.height, p.rgba)
                                })
                                .collect::<Vec<_>>()
                        })
                })
                .await;

            let updated = this.update(cx, |this, cx| {
                if this.generation != generation {
                    trace!("preview: dropping stale render (gen {generation})");
                    return; // a newer render is already in flight
                }
                match result {
                    Ok(pages) => {
                        this.pages = pages
                            .into_iter()
                            .filter_map(|(width, height, bgra)| {
                                RgbaImage::from_raw(width, height, bgra).map(|buf| Page {
                                    image: Arc::new(RenderImage::new([Frame::new(buf)])),
                                    width,
                                    height,
                                })
                            })
                            .collect();
                        debug!("preview rendered — {} page(s)", this.pages.len());
                        this.status = Status::Ready;
                    }
                    Err(err) => {
                        warn!("preview render failed: {err}");
                        this.status = Status::Failed(err.to_string());
                    }
                }
                cx.notify();
            });
            if updated.is_err() {
                trace!("preview window went away before its render finished");
            }
        }));
    }

    fn status_line(&self) -> String {
        match &self.status {
            Status::Idle => "Kein Wettkampf ausgewählt".to_owned(),
            Status::Rendering => "Wird gerendert …".to_owned(),
            Status::Refreshing => "Aktualisiere …".to_owned(),
            Status::Ready => match self.pages.len() {
                1 => "1 Seite".to_owned(),
                n => format!("{n} Seiten"),
            },
            Status::Failed(err) => format!("Fehler: {err}"),
        }
    }
}

impl gpui::Render for PreviewWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let failed = matches!(self.status, Status::Failed(_));

        // Fit each page to the viewport width (minus padding + scrollbar gutter).
        let viewport_w: f32 = window.viewport_size().width.into();
        let content_px = (viewport_w - 72.).max(240.);
        let content_w = px(content_px);
        let ground = hsla(c.background.h, c.background.s * 0.4, c.background.l, 1.0);
        let ground = if c.background.l > 0.5 {
            hsla(ground.h, ground.s, 0.90, 1.0)
        } else {
            hsla(ground.h, ground.s, 0.16, 1.0)
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(c.background)
            .text_color(c.foreground)
            .child(
                // Status strip.
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .when_else(
                        cfg!(target_os = "macos"),
                        |el| el.pl(crate::skin::titlebar::content_leading_inset(window)),
                        |el| el.px(px(16.)),
                    )
                    .h(px(34.))
                    .border_b_1()
                    .border_color(c.border)
                    .bg(c.chrome)
                    .text_size(px(12.))
                    .text_color(if failed {
                        c.critical
                    } else {
                        c.muted_foreground
                    })
                    .child(self.status_line()),
            )
            .child(
                div()
                    .id("preview-scroll")
                    .flex_1()
                    .min_h(px(0.))
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .bg(ground)
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(18.))
                    .p(px(28.))
                    .when(self.pages.is_empty() && !failed, |el| {
                        el.child(
                            div()
                                .mt(px(80.))
                                .text_size(px(13.))
                                .text_color(c.muted_foreground)
                                .child(match self.status {
                                    Status::Idle => {
                                        "Wähle einen Wettkampf, um die Vorschau zu sehen."
                                    }
                                    _ => "Wird gerendert …",
                                }),
                        )
                    })
                    .children(self.pages.iter().map(|page| {
                        let scale = content_px / page.width as f32;
                        let display_h = px(page.height as f32 * scale);
                        div()
                            .flex_none()
                            .w(content_w)
                            .h(display_h)
                            .bg(white())
                            .border_1()
                            .border_color(c.border)
                            .shadow_md()
                            .child(img(page.image.clone()).size_full())
                    })),
            )
    }
}

/// `WindowOptions` for the preview — a plain utility window.
pub fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(880.), px(1000.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("DTB Kampfrichtereinsatzpläne · Vorschau".into()),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        ..Default::default()
    }
}
