//! Secondary "windows" presented as sheets inside the main window.
//!
//! iPadOS apps have a single window scene, so About, Settings, the log viewer, the feedback forms
//! and the trash cannot be separate OS windows there. On such platforms
//! ([`crate::skin::window::secondary_windows_as_sheets`]) each of those views is instead pushed
//! onto a stack owned by a gpui global and drawn by [`overlay`] over the main window: a fixed-size
//! panel with a title bar (Back when a sheet sits on another, Done to dismiss them all) on a
//! dimming scrim. The views themselves are the same ones the desktop windows use.

use gpui_kit::{
    AnyView, App, BorrowAppContext, Entity, FontWeight, Global, InteractiveElement, IntoElement,
    MouseButton,
    ParentElement, Pixels, Render, SharedString, Size, Styled, Window, WindowBounds, WindowOptions,
    deferred, div, prelude::FluentBuilder, px, relative,
};

use crate::components::button::{Button, ButtonTone};
use crate::i18n::ActiveLocale;
use crate::theme::ActiveTheme;

/// Above dialogs (which sit at 10) and below popovers (`gpui_kit::base::POPUP_PRIORITY`, 100), so
/// a dropdown opened inside a sheet still floats over it.
const SHEET_PRIORITY: usize = 50;

/// Whether secondary windows are sheets on this platform.
pub const fn enabled() -> bool {
    crate::skin::window::secondary_windows_as_sheets()
}

struct Sheet {
    key: &'static str,
    title: SharedString,
    size: Size<Pixels>,
    view: AnyView,
}

/// The presented sheets, bottom to top.
#[derive(Default)]
pub struct Sheets {
    stack: Vec<Sheet>,
}

impl Global for Sheets {}

pub fn init(cx: &mut App) {
    cx.set_global(Sheets::default());
}

/// Present a sheet, taking its title and size from the `WindowOptions` the desktop window would
/// have used. `key` makes it a singleton: presenting a key that is already on the stack brings
/// that sheet to the top (dropping anything above it) instead of stacking a second one.
///
/// The view is built inside the main window, deferred until the current update finishes — the
/// caller is often a menu action that is itself running inside that window.
pub fn present<V: Render>(
    cx: &mut App,
    key: &'static str,
    options: &WindowOptions,
    build: impl FnOnce(&mut Window, &mut App) -> Entity<V> + 'static,
) {
    let title = options
        .titlebar
        .as_ref()
        .and_then(|titlebar| titlebar.title.clone())
        .unwrap_or_default();
    let size = match options.window_bounds {
        Some(WindowBounds::Windowed(bounds)) => bounds.size,
        _ => Size::new(px(640.), px(520.)),
    };

    cx.defer(move |cx| {
        let existing = cx
            .global::<Sheets>()
            .stack
            .iter()
            .position(|sheet| sheet.key == key);
        if let Some(index) = existing {
            cx.update_global::<Sheets, _>(|sheets, _| sheets.stack.truncate(index + 1));
            return;
        }
        let Some(main) = crate::app::main_window_handle() else {
            log::warn!("cannot present the {key} sheet: there is no main window");
            return;
        };
        let mut build = Some(build);
        let built = main.update(cx, |_, window, cx| build.take().map(|build| build(window, cx)));
        match built {
            Ok(Some(view)) => cx.update_global::<Sheets, _>(|sheets, _| {
                sheets.stack.push(Sheet {
                    key,
                    title,
                    size,
                    view: view.into(),
                });
            }),
            _ => log::error!("failed to build the {key} sheet"),
        }
    });
}

/// Dismiss the top sheet.
pub fn dismiss(cx: &mut App) {
    cx.update_global::<Sheets, _>(|sheets, _| {
        sheets.stack.pop();
    });
}

fn dismiss_all(cx: &mut App) {
    cx.update_global::<Sheets, _>(|sheets, _| sheets.stack.clear());
}

/// Close whatever hosts the calling view: its own window on the desktop, the top sheet here.
pub fn close(window: &mut Window, cx: &mut App) {
    if enabled() {
        dismiss(cx);
    } else {
        window.remove_window();
    }
}

/// The sheet layer for `AppShell` to paint last, or `None` when no sheet is presented.
pub fn overlay(window: &Window, cx: &mut App) -> Option<impl IntoElement> {
    let sheets = cx.try_global::<Sheets>()?;
    let top = sheets.stack.last()?;
    let (title, size, view) = (top.title.clone(), top.size, top.view.clone());
    let stacked = sheets.stack.len() > 1;

    let c = cx.theme().color;
    let radius = cx.theme().skin.radius_lg_px();
    let corner_inset = px((f32::from(radius) * (1. - std::f32::consts::FRAC_1_SQRT_2)).ceil() + 1.);
    let insets = crate::skin::window::content_insets(window);
    let (back_label, done_label) = (cx.t("sheet.back"), cx.t("sheet.done"));

    let header = div()
        .flex()
        .flex_none()
        .items_center()
        .h(px(48.))
        .px(px(10.))
        .border_b_1()
        .border_color(c.border)
        .bg(c.chrome)
        // gpui clips to rectangles, so the header rounds its own top corners.
        .rounded_t(radius - px(1.))
        .child(
            div().w(px(110.)).flex().justify_start().when(stacked, |el| {
                el.child(
                    Button::new("sheet-back", back_label)
                        .tone(ButtonTone::Ghost)
                        .small()
                        .on_click(|_, _, cx| dismiss(cx)),
                )
            }),
        )
        .child(
            div()
                .flex_1()
                .min_w(px(0.))
                .text_center()
                .truncate()
                .font_weight(FontWeight::SEMIBOLD)
                .child(title),
        )
        .child(
            div().w(px(110.)).flex().justify_end().child(
                Button::new("sheet-done", done_label)
                    .tone(ButtonTone::Primary)
                    .small()
                    .on_click(|_, _, cx| dismiss_all(cx)),
            ),
        );

    let panel = div()
        .occlude()
        .flex()
        .flex_col()
        .w(size.width)
        .h(size.height)
        .max_w(relative(0.96))
        .max_h(relative(0.94))
        .overflow_hidden()
        .rounded(radius)
        .border_1()
        .border_color(c.border)
        .bg(c.background)
        .text_color(c.foreground)
        .shadow_lg()
        .child(header)
        // Clipping is rectangular, so a view's square corners would poke out of the rounded
        // panel. Insetting the content by the depth of the corner arc (r·(1−1/√2), rounded up)
        // keeps them inside; the panel's background matches the views', so the gutter is
        // invisible.
        .child(
            div()
                .relative()
                .flex_1()
                .min_h(px(0.))
                .mx(corner_inset)
                .mb(corner_inset)
                .child(view),
        );

    Some(deferred(
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .pt(insets.top)
            .pr(insets.right)
            .pb(insets.bottom)
            .pl(insets.left)
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .bg(c.overlay)
                    .on_mouse_down(MouseButton::Left, |_, _, cx| dismiss(cx)),
            )
            .child(panel),
    )
    .with_priority(SHEET_PRIORITY))
}
