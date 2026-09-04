//! The "Bug melden" / "Funktion anfragen" windows.
//!
//! A small form — a one-line summary, a multi-line description, an optional
//! contact address, and (for bug reports) an "attach session log" box — mailed
//! to the maintainer via [`crate::mail`], the exact transport the crash reporter
//! uses. The report body carries the build metadata (version / profile / target)
//! so an issue can be placed without asking.
//!
//! When the build has no SMTP credentials ([`mail::available`] is `false`) the
//! form is replaced by a pointer to the public issue tracker.
//!
//! At most one window of each kind is open at a time; re-triggering the action
//! just brings the existing one forward.

use std::cell::RefCell;
use std::collections::HashMap;
use std::time::Duration;

use gpui::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, ParentElement, Render, Size, StatefulInteractiveElement,
    Styled, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div,
    prelude::FluentBuilder, px,
};
use gpui_base::input::{InputState, Textarea, TextareaState};

use crate::actions::REPOSITORY_URL;
use crate::build_info;
use crate::components::checkbox::Checkbox;
use crate::components::field::Field;
use crate::components::{Button, ButtonTone, Icon, Spinner};
use crate::filesystem::FilesystemHelper;
use crate::mail;
use crate::theme::ActiveTheme;

/// Which of the two feedback forms this window is.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Kind {
    Bug,
    Feature,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::Bug => "Bug melden",
            Kind::Feature => "Funktion anfragen",
        }
    }

    /// The bracketed tag that opens the e-mail subject.
    fn subject_tag(self) -> &'static str {
        match self {
            Kind::Bug => "Bug",
            Kind::Feature => "Funktionswunsch",
        }
    }

    fn intro(self) -> &'static str {
        match self {
            Kind::Bug => {
                "Beschreibe das Problem möglichst genau. Der Bericht enthält zusätzlich die \
                 Programmversion und technische Angaben zum Build – keine Wettkampfdaten."
            }
            Kind::Feature => {
                "Beschreibe die gewünschte Funktion und wozu sie dienen soll. Der Bericht enthält \
                 zusätzlich die Programmversion."
            }
        }
    }

    fn summary_placeholder(self) -> &'static str {
        match self {
            Kind::Bug => "Kurze Zusammenfassung des Problems",
            Kind::Feature => "Kurze Zusammenfassung der Idee",
        }
    }

    fn description_placeholder(self) -> &'static str {
        match self {
            Kind::Bug => {
                "Was ist passiert? Was hättest du erwartet? Schritte zur Reproduktion, falls bekannt."
            }
            Kind::Feature => "Was möchtest du können, und warum wäre das hilfreich?",
        }
    }

    /// Only bug reports offer to attach the session log.
    fn offers_log(self) -> bool {
        matches!(self, Kind::Bug)
    }

    fn size(self) -> Size<gpui::Pixels> {
        Size::new(px(520.), px(560.))
    }
}

thread_local! {
    static OPEN: RefCell<HashMap<Kind, AnyWindowHandle>> = RefCell::new(HashMap::new());
}

/// Open (or focus) the bug-report window. Entry point for `help::ReportBug`.
pub fn open_bug(cx: &mut App) {
    open_kind(cx, Kind::Bug);
}

/// Open (or focus) the feature-request window. Entry point for
/// `help::RequestFeature`.
pub fn open_feature(cx: &mut App) {
    open_kind(cx, Kind::Feature);
}

fn open_kind(cx: &mut App, kind: Kind) {
    let existing = OPEN.with(|m| m.borrow().get(&kind).copied());
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
        OPEN.with(|m| {
            m.borrow_mut().remove(&kind);
        });
    }

    let opts = window_options(kind, cx);
    match cx.open_window(opts, |window, cx| {
        cx.new(|cx| FeedbackWindow::new(kind, window, cx))
    }) {
        Ok(handle) => {
            OPEN.with(|m| {
                m.borrow_mut().insert(kind, handle.into());
            });
            log::info!("opened the {} window", kind.subject_tag());
        }
        Err(err) => log::error!(
            "{}-Fenster konnte nicht geöffnet werden: {err}",
            kind.title()
        ),
    }
}

fn window_options(kind: Kind, cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            kind.size(),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(kind.title().into()),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_resizable: true,
        is_minimizable: false,
        window_min_size: Some(Size::new(px(420.), px(440.))),
        ..Default::default()
    }
}

#[derive(Clone)]
enum SendState {
    Idle,
    Sending,
    /// Delivered — "Gesendet!" is shown, then the window closes.
    Sent,
    Failed(String),
}

impl SendState {
    fn busy(&self) -> bool {
        matches!(self, SendState::Sending | SendState::Sent)
    }
}

pub struct FeedbackWindow {
    kind: Kind,
    focus: FocusHandle,
    summary: Entity<InputState>,
    description: Entity<TextareaState>,
    contact: Entity<InputState>,
    /// "Protokoll anhängen" — **off** by default, bug reports only.
    attach_log: bool,
    state: SendState,
}

impl Drop for FeedbackWindow {
    fn drop(&mut self) {
        OPEN.with(|m| {
            m.borrow_mut().remove(&self.kind);
        });
    }
}

impl FeedbackWindow {
    fn new(kind: Kind, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let summary =
            cx.new(|cx| InputState::new(window, cx).placeholder(kind.summary_placeholder()));
        let description = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(kind.description_placeholder())
                .auto_grow(6, 18)
        });
        let contact = cx
            .new(|cx| InputState::new(window, cx).placeholder("E-Mail für Rückfragen (optional)"));

        summary.update(cx, |state, cx| state.focus(window, cx));

        Self {
            kind,
            focus: cx.focus_handle(),
            summary,
            description,
            contact,
            attach_log: false,
            state: SendState::Idle,
        }
    }

    fn can_send(&self, cx: &App) -> bool {
        !self.state.busy()
            && !self.summary.read(cx).value().trim().is_empty()
            && !self.description.read(cx).value().trim().is_empty()
    }

    /// Mail the report on the background executor; on success show "Gesendet!"
    /// briefly, then close the window. On failure surface the error and stay
    /// open.
    fn send(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.can_send(cx) {
            return;
        }
        self.state = SendState::Sending;
        cx.notify();

        let summary = self.summary.read(cx).value().trim().to_string();
        let description = self.description.read(cx).value().trim().to_string();
        let contact = self.contact.read(cx).value().trim().to_string();
        let (subject, body) = compose(self.kind, &summary, &description, &contact);

        let mut attachments = Vec::new();
        if self.kind.offers_log() && self.attach_log {
            let log = session_log_tail();
            if !log.is_empty() {
                attachments.push(("session.log".to_owned(), "text/plain; charset=utf-8", log));
            }
        }

        let report = mail::Report {
            subject,
            body,
            attachments,
        };

        cx.spawn_in(window, async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { mail::send(report) })
                .await;
            match result {
                Ok(()) => {
                    log::info!("feedback report sent");
                    this.update(cx, |this, cx| {
                        this.state = SendState::Sent;
                        cx.notify();
                    })
                    .ok();
                    cx.background_executor().timer(Duration::from_secs(4)).await;
                    this.update_in(cx, |_, window, _| window.remove_window())
                        .ok();
                }
                Err(err) => {
                    log::warn!("feedback report could not be sent: {err}");
                    this.update(cx, |this, cx| {
                        this.state = SendState::Failed(err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

impl Focusable for FeedbackWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Render for FeedbackWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();

        div()
            .track_focus(&self.focus)
            .key_context("FeedbackWindow")
            .size_full()
            .bg(c.background)
            .text_color(c.foreground)
            .flex()
            .flex_col()
            .p(px(20.))
            .when_else(
                cfg!(target_os = "macos"),
                |el| el.pt(px(40.)),
                |el| el.pt(px(20.)),
            )
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(self.kind.title()),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(self.kind.intro()),
            )
            .map(|el| {
                if mail::available() {
                    el.child(self.form(radius, c, cx))
                } else {
                    el.child(self.unavailable_notice(c))
                }
            })
    }
}

impl FeedbackWindow {
    fn form(
        &self,
        radius: gpui::Pixels,
        c: crate::theme::PaletteColors,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.state.busy();
        let sent = matches!(self.state, SendState::Sent);
        let failed = matches!(self.state, SendState::Failed(_));
        let can_send = self.can_send(cx);
        let weak = cx.entity().downgrade();

        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(field_label("Zusammenfassung", c))
            .child(Field::new("fb-summary", &self.summary).paints_background(true))
            .child(field_label("Beschreibung", c))
            .child(
                div()
                    .flex_1()
                    .min_h(px(120.))
                    .rounded(radius)
                    .border_1()
                    .border_color(c.border)
                    .bg(c.surface)
                    .px(px(8.))
                    .py(px(6.))
                    .text_size(px(13.))
                    .child(Textarea::new(&self.description)),
            )
            .child(field_label("Kontakt", c))
            .child(Field::new("fb-contact", &self.contact).paints_background(true))
            .when(self.kind.offers_log(), |el| {
                let row_weak = weak.clone();
                el.child(
                    div()
                        .id("fb-attach-log")
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .when(!busy, |el| {
                            el.cursor_pointer().on_click(move |_, _window, cx| {
                                row_weak
                                    .update(cx, |this, cx| {
                                        if !this.state.busy() {
                                            this.attach_log = !this.attach_log;
                                            cx.notify();
                                        }
                                    })
                                    .ok();
                            })
                        })
                        .child(
                            Checkbox::new(("fb-attach-log", 1u32), self.attach_log).disabled(busy),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(c.muted_foreground)
                                .child(
                                    "Protokoll dieser Sitzung anhängen (enthält Wettkampfnamen)",
                                ),
                        ),
                )
            })
            .when(failed, |el| {
                let SendState::Failed(msg) = &self.state else {
                    return el;
                };
                el.child(
                    div()
                        .mt(px(2.))
                        .text_size(px(11.))
                        .text_color(c.critical)
                        .child(msg.clone()),
                )
            })
            .child(
                div()
                    .mt(px(4.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .when(sent, |el| {
                        el.child(
                            div()
                                .flex()
                                .items_center()
                                .gap(px(6.))
                                .text_size(px(13.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(c.ok)
                                .child(Icon::Check.size(px(15.)).color(c.ok))
                                .child("Gesendet! Vielen Dank."),
                        )
                    })
                    .when(matches!(self.state, SendState::Sending), |el| {
                        el.child(Spinner::new().size(px(13.))).child(
                            div()
                                .flex_1()
                                .text_size(px(11.))
                                .text_color(c.muted_foreground)
                                .child("Wird gesendet …"),
                        )
                    })
                    .when(!sent, |el| {
                        el.child(
                            Button::new("fb-cancel", "Abbrechen")
                                .disabled(busy)
                                .on_click(|_, window, _| window.remove_window()),
                        )
                        .child({
                            let send_weak = weak.clone();
                            Button::new("fb-send", if failed { "Erneut senden" } else { "Senden" })
                                .tone(ButtonTone::Primary)
                                .disabled(!can_send)
                                .on_click(move |_, window, cx| {
                                    send_weak.update(cx, |this, cx| this.send(window, cx)).ok();
                                })
                        })
                    }),
            )
    }

    fn unavailable_notice(&self, c: crate::theme::PaletteColors) -> impl IntoElement {
        div()
            .flex_1()
            .flex()
            .flex_col()
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(
                        "Der Versand von Rückmeldungen ist in dieser Programmversion nicht möglich. \
                         Bitte nutze den öffentlichen Issue-Tracker auf Codeberg.",
                    ),
            )
            .child(
                div()
                    .flex()
                    .justify_end()
                    .gap(px(8.))
                    .child(
                        Button::new("fb-close", "Schließen")
                            .on_click(|_, window, _| window.remove_window()),
                    )
                    .child(
                        Button::new("fb-codeberg", "Auf Codeberg öffnen")
                            .tone(ButtonTone::Primary)
                            .on_click(|_, _, cx| cx.open_url(REPOSITORY_URL)),
                    ),
            )
    }
}

fn field_label(text: &'static str, c: crate::theme::PaletteColors) -> impl IntoElement {
    div()
        .text_size(px(11.))
        .font_weight(FontWeight::SEMIBOLD)
        .text_color(c.muted_foreground)
        .child(text)
}

/// Build the e-mail subject + plain-text body. The body leads with the build
/// metadata so an issue can be triaged without a round-trip.
fn compose(kind: Kind, summary: &str, description: &str, contact: &str) -> (String, String) {
    let short: String = summary.chars().take(80).collect();
    let subject = format!(
        "[{}] {} · DTB KE v{}",
        kind.subject_tag(),
        short,
        build_info::APP_VERSION
    );

    let mut body = String::new();
    body.push_str(&format!(
        "DTB Kampfrichtereinsatzpläne {} ({})\n",
        build_info::APP_VERSION,
        build_info::PROFILE
    ));
    body.push_str(&format!(
        "Ziel: {} · {}\n",
        build_info::TARGET_TRIPLE,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    ));
    body.push_str(&format!("Art: {}\n", kind.subject_tag()));
    body.push_str(&format!(
        "Kontakt: {}\n",
        if contact.is_empty() { "—" } else { contact }
    ));
    body.push_str(&format!("\nZusammenfassung: {summary}\n\n"));
    body.push_str(description.trim_end());
    body.push('\n');
    (subject, body)
}

/// The current session log, tail-capped at 4 MB on a line boundary.
fn session_log_tail() -> Vec<u8> {
    const CAP: usize = 1 << 22;
    let path = FilesystemHelper::instance().get_log_file();
    let Ok(bytes) = std::fs::read(path) else {
        return Vec::new();
    };
    if bytes.len() <= CAP {
        return bytes;
    }
    let start = bytes.len() - CAP;
    let start = bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|nl| start + nl + 1)
        .unwrap_or(start);
    let mut tail = format!(
        "… (gekürzt, {} von {} Bytes)\n",
        bytes.len() - start,
        bytes.len()
    )
    .into_bytes();
    tail.extend_from_slice(&bytes[start..]);
    tail
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subject_carries_the_kind_tag_and_is_trimmed() {
        let (subject, _) = compose(Kind::Bug, &"x".repeat(200), "desc", "");
        assert!(subject.starts_with("[Bug] "));
        assert!(subject.contains(&format!("DTB KE v{}", build_info::APP_VERSION)));
        // 80 summary chars + the fixed chrome, nothing like 200.
        assert!(subject.len() < 140);
    }

    #[test]
    fn body_has_metadata_and_the_contact_placeholder() {
        let (_, body) = compose(Kind::Feature, "Summe", "Bitte einen Dunkelmodus", "");
        assert!(body.contains("Art: Funktionswunsch"));
        assert!(body.contains("Kontakt: —"));
        assert!(body.contains("Bitte einen Dunkelmodus"));
    }

    #[test]
    fn contact_is_included_when_given() {
        let (_, body) = compose(Kind::Bug, "s", "d", "kr@example.org");
        assert!(body.contains("Kontakt: kr@example.org"));
    }
}
