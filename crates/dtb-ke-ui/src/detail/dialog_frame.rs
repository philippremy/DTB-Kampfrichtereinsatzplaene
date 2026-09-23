//! Shared chrome for our modal dialogs (rendered inside a `gpui_kit::base::Dialog`).

use gpui_kit::{App, ClickEvent, Context, Div, InteractiveElement, Pixels, Styled, Window, div, px};

use crate::theme::Theme;

/// A `Dialog::on_cancel` handler (must return `bool`) that routes escape /
/// backdrop dismissal through the view's own `close` method.
pub fn dismiss_handler<T: 'static>(
    cx: &Context<T>,
    close: impl Fn(&mut T, &mut Context<T>) + 'static,
) -> impl Fn(&ClickEvent, &mut Window, &mut App) -> bool + 'static {
    let weak = cx.weak_entity();
    move |_, _window, cx| {
        weak.update(cx, |this, cx| close(this, cx)).ok();
        true
    }
}

/// The dimming layer behind a dialog. Must be `absolute` + fill the host.
pub fn scrim(theme: &Theme) -> Div {
    div().absolute().inset_0().bg(theme.color.overlay)
}

/// A styled dialog panel — a surface card `width` wide, laid out as a column.
///
/// `.occlude()` stops mouse events inside the panel from reaching the
/// `gpui_kit::base::Dialog` backdrop (whose `on_any_mouse_down` dismisses the dialog
/// when its hitbox is the front-most one under the cursor).
pub fn dialog_panel(theme: &Theme, width: Pixels) -> Div {
    div()
        .occlude()
        .flex()
        .flex_col()
        .gap(px(10.))
        .w(width)
        .p(px(18.))
        .rounded(theme.skin.radius_lg_px())
        .border_1()
        .border_color(theme.color.border)
        .bg(theme.color.surface)
        .text_color(theme.color.foreground)
        .shadow_lg()
}
