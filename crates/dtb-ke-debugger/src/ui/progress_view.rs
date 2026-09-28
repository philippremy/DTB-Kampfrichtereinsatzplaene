//! Progress cards: what the debugger shows while it works — the loading screen (the current stage, plus the download
//! inside it) and the source pane while a file is fetched. Everything here is drawn from a [`Snapshot`], so a bar
//! only fills as far as the numbers behind it say.

use crate::progress::{Snapshot, TransferSnapshot};
use dtb_ke_ui::components::ProgressBar;
use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::{
    AnyElement, App, IntoElement, ParentElement, SharedString, Styled, div, prelude::FluentBuilder,
    px,
};

/// One bar with its title above and a detail line below: `summary` on the left, the current item on the right.
fn bar_block(
    title: String,
    fraction: Option<f32>,
    summary: String,
    current: Option<String>,
    cx: &App,
) -> AnyElement {
    let c = cx.theme().color;
    let mono = cx.theme().skin.mono_font_family();
    div()
        .flex()
        .flex_col()
        .w_full()
        .gap(px(6.))
        .child(
            div()
                .text_size(px(13.))
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .child(SharedString::from(title)),
        )
        .child(ProgressBar::new(fraction))
        .child(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .text_size(px(11.5))
                .text_color(c.muted_foreground)
                .child(div().flex_none().child(SharedString::from(summary)))
                .when_some(current, |el, current| {
                    el.child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .text_right()
                            .font_family(mono)
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(current)),
                    )
                }),
        )
        .into_any_element()
}

fn transfer_block(t: &TransferSnapshot, cx: &App) -> AnyElement {
    bar_block(
        format!("Downloading {}", t.label),
        t.fraction(),
        t.summary(),
        None,
        cx,
    )
}

/// The loading screen: the analysis stage (`Step 2 of 4 — Loading symbols`, `n / m`, percentage, ETA) and, while a
/// debug file downloads, a second bar with its bytes, speed and time left.
pub fn loading_card(snapshot: &Snapshot, cx: &App) -> AnyElement {
    let title = if snapshot.stage.is_empty() {
        "Starting …".to_owned()
    } else {
        snapshot.title()
    };
    let current = (!snapshot.label.is_empty()).then(|| snapshot.label.clone());
    div()
        .flex()
        .flex_col()
        .w(px(520.))
        .max_w_full()
        .gap(px(18.))
        .child(bar_block(
            title,
            snapshot.fraction(),
            snapshot.summary(),
            current,
            cx,
        ))
        .when_some(snapshot.transfer.as_ref(), |el, t| {
            el.child(transfer_block(t, cx))
        })
        .into_any_element()
}

/// The source pane while a file is fetched: `title` says what and from where; the bar is the download's bytes when a
/// crate archive is coming down, otherwise the steps of the git read (commit, directories, file).
pub fn fetch_card(title: String, snapshot: &Snapshot, cx: &App) -> AnyElement {
    let (fraction, summary) = match &snapshot.transfer {
        Some(t) => (t.fraction(), t.summary()),
        None => (snapshot.fraction(), snapshot.summary()),
    };
    let current = match &snapshot.transfer {
        Some(t) => Some(t.label.clone()),
        None => (!snapshot.label.is_empty()).then(|| snapshot.label.clone()),
    };
    div()
        .flex()
        .flex_col()
        .w_full()
        .p(px(24.))
        .child(bar_block(title, fraction, summary, current, cx))
        .into_any_element()
}
