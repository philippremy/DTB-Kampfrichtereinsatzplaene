//! The corner update notification.
//!
//! A collapsed pill anchored bottom-right of the shell; clicking it opens a card
//! with the release details and the Skip / Later / Install actions (Ghostty's
//! layout, our chrome). `AppShell` owns the [`Updater`](super::Updater) entity,
//! reads its [`State`], and supplies the callbacks. Themed entirely through
//! tokens, so Windows / Linux get the same card as macOS.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, FontWeight, InteractiveElement, IntoElement, ParentElement, RenderOnce,
    StatefulInteractiveElement, Styled, Window, div, prelude::FluentBuilder, px,
};

use super::{Release, State};
use crate::components::icon::Icon;
use crate::components::{Button, ButtonTone, ProgressBar, Spinner};
use crate::i18n::ActiveLocale;
use crate::theme::ActiveTheme;

type Cb = Rc<dyn Fn(&mut Window, &mut App)>;

#[derive(IntoElement)]
pub struct UpdaterToast {
    state: State,
    expanded: bool,
    /// Whether the current channel/platform combination can self-install (see
    /// `updater::can_self_install`) — decides the primary button's label/icon.
    can_self_install: bool,
    on_toggle: Cb,
    on_skip: Cb,
    on_later: Cb,
    /// "Install & relaunch" (self-install platforms) or "Download" (elsewhere).
    on_primary: Cb,
    on_notes: Cb,
    on_dismiss: Cb,
    /// A click outside the expanded card — collapses back to the pill.
    on_outside_click: Cb,
}

impl UpdaterToast {
    pub fn new(state: State, expanded: bool, can_self_install: bool) -> Self {
        let noop: Cb = Rc::new(|_, _| {});
        Self {
            state,
            expanded,
            can_self_install,
            on_toggle: noop.clone(),
            on_skip: noop.clone(),
            on_later: noop.clone(),
            on_primary: noop.clone(),
            on_notes: noop.clone(),
            on_dismiss: noop.clone(),
            on_outside_click: noop,
        }
    }

    pub fn on_toggle(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle = Rc::new(f);
        self
    }
    pub fn on_skip(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_skip = Rc::new(f);
        self
    }
    pub fn on_later(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_later = Rc::new(f);
        self
    }
    pub fn on_primary(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_primary = Rc::new(f);
        self
    }
    pub fn on_notes(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_notes = Rc::new(f);
        self
    }
    pub fn on_dismiss(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_dismiss = Rc::new(f);
        self
    }
    pub fn on_outside_click(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_outside_click = Rc::new(f);
        self
    }
}

impl RenderOnce for UpdaterToast {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_lg_px();
        let expanded = self.expanded;
        let outside = self.on_outside_click.clone();

        div()
            .id("updater-toast")
            .absolute()
            .bottom(px(12.))
            .right(px(12.))
            .flex()
            .flex_col()
            .items_end()
            .gap(px(6.))
            // Otherwise hover/click on the toast (and the gap between the pill
            // and the expanded card) falls through to whatever is underneath.
            .occlude()
            // A click anywhere outside the pill+card pair collapses the card
            // back to the pill — attached to the whole toast (not just the
            // card) so the pill's own click doesn't double-fire alongside it.
            .when(expanded, |el| {
                el.on_mouse_down_out(move |_, window, cx| outside(window, cx))
            })
            .when(self.expanded, |el| el.child(self.card(c, radius, cx)))
            .child(self.pill(c, cx))
    }
}

impl UpdaterToast {
    /// The always-visible collapsed affordance.
    fn pill(&self, c: crate::theme::PaletteColors, cx: &App) -> impl IntoElement {
        let toggle = self.on_toggle.clone();
        let (icon, label, tone): (Icon, String, PillTone) = match &self.state {
            State::Available(r) => (
                Icon::Package,
                cx.t_fmt("updater.pill-available", &[("version", &r.version)]),
                PillTone::Accent,
            ),
            State::Installing { .. } => (
                Icon::Download,
                cx.t("updater.pill-installing").to_string(),
                PillTone::Accent,
            ),
            State::Restart(_) => (
                Icon::RotateCw,
                cx.t("updater.pill-restart").to_string(),
                PillTone::Accent,
            ),
            State::UpToDate => (
                Icon::Check,
                cx.t("updater.pill-up-to-date").to_string(),
                PillTone::Neutral,
            ),
            State::Failed(_) => (
                Icon::Warning,
                cx.t("updater.pill-failed").to_string(),
                PillTone::Critical,
            ),
            State::Idle | State::Checking => {
                return div().into_any_element();
            }
        };

        let (bg, fg) = match tone {
            PillTone::Accent => (c.primary, c.primary_foreground),
            PillTone::Neutral => (c.surface, c.foreground),
            PillTone::Critical => (c.critical, c.primary_foreground),
        };

        div()
            .id("updater-pill")
            .flex()
            .items_center()
            .gap(px(6.))
            .h(px(26.))
            .px(px(10.))
            .rounded_full()
            .bg(bg)
            .text_color(fg)
            .text_size(px(11.5))
            .font_weight(FontWeight::MEDIUM)
            .shadow_md()
            .cursor_pointer()
            .hover(|el| el.opacity(0.92))
            .on_click(move |_, window, cx| toggle(window, cx))
            .when(matches!(self.state, State::Installing { .. }), |el| {
                el.child(Spinner::new().size(px(12.)).color(fg))
            })
            .when(!matches!(self.state, State::Installing { .. }), |el| {
                el.child(icon.size(px(13.)).color(fg))
            })
            .child(label)
            .into_any_element()
    }

    /// The expanded detail card.
    fn card(
        &self,
        c: crate::theme::PaletteColors,
        radius: gpui_kit::Pixels,
        cx: &App,
    ) -> impl IntoElement {
        let body: AnyElement = match &self.state {
            State::Available(release) => self.available_body(release, c, cx).into_any_element(),
            State::Installing { progress, .. } => {
                self.installing_body(*progress, c, cx).into_any_element()
            }
            State::Restart(_) => self.restart_body(c, cx).into_any_element(),
            State::Failed(msg) => self.failed_body(msg, c, cx).into_any_element(),
            State::UpToDate => div()
                .text_size(px(12.))
                .text_color(c.muted_foreground)
                .child(cx.t("updater.up-to-date-body"))
                .into_any_element(),
            State::Idle | State::Checking => div().into_any_element(),
        };

        div()
            .w(px(320.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(14.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.surface)
            .text_color(c.foreground)
            .shadow_lg()
            .child(
                div()
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(self.card_title(cx)),
            )
            .child(body)
    }

    fn card_title(&self, cx: &App) -> gpui_kit::SharedString {
        let key = match &self.state {
            State::Restart(_) => "updater.title-restart",
            State::Failed(_) => "updater.title-failed",
            State::UpToDate => "updater.title-up-to-date",
            _ => "updater.title-available",
        };
        cx.t(key)
    }

    fn available_body(
        &self,
        release: &Release,
        c: crate::theme::PaletteColors,
        cx: &App,
    ) -> impl IntoElement {
        let skip = self.on_skip.clone();
        let later = self.on_later.clone();
        let primary = self.on_primary.clone();
        let notes = self.on_notes.clone();
        let has_notes = release.notes_url.is_some();

        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .child(detail_row(
                        cx.t("updater.detail-version"),
                        &release.version,
                        c,
                    ))
                    .when_some(release.size, |el, size| {
                        el.child(detail_row(
                            cx.t("updater.detail-size"),
                            &human_size(size),
                            c,
                        ))
                    })
                    .when(!release.date.is_empty(), |el| {
                        el.child(detail_row(
                            cx.t("updater.detail-published"),
                            &release.date,
                            c,
                        ))
                    }),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Button::new("updater-skip", cx.t("updater.skip-button"))
                            .tone(ButtonTone::Ghost)
                            .x_small()
                            .on_click(move |_, window, cx| skip(window, cx)),
                    )
                    .child(
                        Button::new("updater-later", cx.t("updater.later-button"))
                            .tone(ButtonTone::Ghost)
                            .x_small()
                            .on_click(move |_, window, cx| later(window, cx)),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new(
                            "updater-primary",
                            if self.can_self_install {
                                cx.t("updater.install-button")
                            } else {
                                cx.t("updater.download-button")
                            },
                        )
                        .tone(ButtonTone::Primary)
                        .x_small()
                        .leading_icon(if self.can_self_install {
                            Icon::RotateCw
                        } else {
                            Icon::Download
                        })
                        .on_click(move |_, window, cx| primary(window, cx)),
                    ),
            )
            .when(has_notes, |el| {
                el.child(
                    div()
                        .id("updater-notes")
                        .w_full()
                        .flex()
                        .items_center()
                        .gap(px(5.))
                        .pt(px(8.))
                        .border_t_1()
                        .border_color(c.border)
                        .text_size(px(10.))
                        .text_color(c.muted_foreground)
                        .cursor_pointer()
                        .hover(|el| el.text_color(c.foreground))
                        .on_click(move |_, window, cx| notes(window, cx))
                        .child(Icon::Document.size(px(11.)).color(c.muted_foreground))
                        .child(cx.t("updater.notes-button"))
                        .child(div().flex_1())
                        .child(Icon::ExternalLink.size(px(11.)).color(c.muted_foreground)),
                )
            })
    }

    fn installing_body(
        &self,
        progress: Option<f32>,
        c: crate::theme::PaletteColors,
        cx: &App,
    ) -> impl IntoElement {
        let (label, fraction) = match progress {
            Some(f) => (
                cx.t_fmt(
                    "updater.downloading",
                    &[("percent", &((f * 100.0).round() as u32).to_string())],
                ),
                Some(f.clamp(0.0, 1.0)),
            ),
            None => (cx.t("updater.checking-installing").to_string(), None),
        };

        div()
            .flex()
            .flex_col()
            .gap(px(8.))
            .py(px(2.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Spinner::new().size(px(13.)))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(c.muted_foreground)
                            .child(label),
                    ),
            )
            // A real fraction fills the bar; an unknown one (the size is not known yet) slides a segment.
            .child(ProgressBar::new(fraction).height(px(4.)))
    }

    fn restart_body(&self, c: crate::theme::PaletteColors, cx: &App) -> impl IntoElement {
        let primary = self.on_primary.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(cx.t("updater.restart-body")),
            )
            .child(
                div().flex().justify_end().child(
                    Button::new("updater-restart", cx.t("updater.restart-button"))
                        .tone(ButtonTone::Primary)
                        .small()
                        .leading_icon(Icon::RotateCw)
                        .on_click(move |_, window, cx| primary(window, cx)),
                ),
            )
    }

    fn failed_body(&self, msg: &str, c: crate::theme::PaletteColors, cx: &App) -> impl IntoElement {
        let dismiss = self.on_dismiss.clone();
        let notes = self.on_notes.clone();
        div()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .text_size(px(11.5))
                    .text_color(c.critical)
                    .child(msg.to_owned()),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .child(
                        Button::new("updater-dismiss", cx.t("updater.dismiss-button"))
                            .tone(ButtonTone::Ghost)
                            .small()
                            .on_click(move |_, window, cx| dismiss(window, cx)),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("updater-open", cx.t("updater.open-page-button"))
                            .tone(ButtonTone::Secondary)
                            .small()
                            .leading_icon(Icon::ExternalLink)
                            .on_click(move |_, window, cx| notes(window, cx)),
                    ),
            )
    }
}

enum PillTone {
    Accent,
    Neutral,
    Critical,
}

fn detail_row(
    label: impl Into<gpui_kit::SharedString>,
    value: &str,
    c: crate::theme::PaletteColors,
) -> impl IntoElement {
    div()
        .flex()
        .gap(px(8.))
        .text_size(px(10.))
        .child(
            div()
                .flex_none()
                .w(px(84.))
                .text_color(c.muted_foreground)
                .child(label.into()),
        )
        .child(div().child(value.to_owned()))
}

/// `35_651_584` → `"34,0 MB"` (one decimal, German comma).
fn human_size(bytes: u64) -> String {
    const MB: f64 = 1024.0 * 1024.0;
    let mb = bytes as f64 / MB;
    if mb >= 1.0 {
        format!("{:.1} MB", mb).replace('.', ",")
    } else {
        format!("{} KB", (bytes as f64 / 1024.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::human_size;

    #[test]
    fn human_size_reads_in_megabytes() {
        assert_eq!(human_size(35_651_584), "34,0 MB");
        assert_eq!(human_size(1_500_000), "1,4 MB");
        assert_eq!(human_size(4_096), "4 KB");
    }
}
