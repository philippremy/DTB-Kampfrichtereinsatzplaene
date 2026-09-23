//! A small floating action menu, anchored at an arbitrary window-absolute
//! point (a right-click's position) rather than a trigger element's captured
//! bounds — used for the sidebar's competition rows and the judging-table
//! cards. Follows the same `deferred(anchored()) + POPUP_PRIORITY` convention
//! as the app's other popovers (see `detail/judging_table.rs::discipline_selector`),
//! but the dismiss boundary is the popover's own hitbox rather than an
//! ancestor trigger's, since there is no fixed trigger element here.

use std::rc::Rc;

use gpui_kit::{
    AnyElement, App, ElementId, Hsla, InteractiveElement, IntoElement, MouseButton, ParentElement,
    Pixels, Point, Styled, Window, anchored, deferred, div, prelude::FluentBuilder, px,
};

use crate::components::icon::Icon;
use crate::theme::ActiveTheme;

/// An item's click handler, or the menu's outside-click / after-choice
/// dismiss callback.
pub type ContextMenuHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// One row of a [`context_menu`].
#[derive(Clone)]
pub struct ContextMenuItem {
    id: &'static str,
    label: gpui_kit::SharedString,
    icon: Icon,
    danger: bool,
    /// Draws a divider above this row — for setting a destructive action
    /// apart from the rest (e.g. "Delete", at the end of the list).
    separated: bool,
    on_click: ContextMenuHandler,
}

impl ContextMenuItem {
    pub fn new(
        id: &'static str,
        label: impl Into<gpui_kit::SharedString>,
        icon: Icon,
        on_click: impl Fn(&mut Window, &mut App) + 'static,
    ) -> Self {
        Self {
            id,
            label: label.into(),
            icon,
            danger: false,
            separated: false,
            on_click: Rc::new(on_click),
        }
    }

    /// Tint the row red — for a destructive action (e.g. "Delete").
    pub fn danger(mut self) -> Self {
        self.danger = true;
        self
    }

    /// Draw a divider above this row.
    pub fn separated(mut self) -> Self {
        self.separated = true;
        self
    }
}

/// Build the floating menu at `anchor` (a window-absolute point, typically a
/// right-click's position). `on_dismiss` fires both on an outside click and
/// right after an item is chosen, so the caller only needs to clear its own
/// "open" state from one place.
pub fn context_menu(
    list_id: impl Into<ElementId>,
    anchor: Point<Pixels>,
    items: Vec<ContextMenuItem>,
    on_dismiss: ContextMenuHandler,
    cx: &mut App,
) -> AnyElement {
    let theme = cx.theme();
    let c = theme.color;
    let radius = theme.skin.radius_control_px();

    let mut col = div()
        .id(list_id)
        .min_w(px(180.))
        .flex()
        .flex_col()
        .py(px(4.))
        .rounded(radius)
        .border_1()
        .border_color(c.border)
        .bg(c.surface)
        .shadow_lg()
        .occlude()
        .on_mouse_down_out({
            let on_dismiss = on_dismiss.clone();
            move |_, window, cx| on_dismiss(window, cx)
        });

    for item in items {
        let on_click = item.on_click.clone();
        let on_dismiss = on_dismiss.clone();
        let fg = if item.danger { c.critical } else { c.foreground };
        let icon_fg = if item.danger {
            c.critical
        } else {
            c.muted_foreground
        };
        let hover_bg = if item.danger {
            Hsla {
                a: 0.12,
                ..c.critical
            }
        } else {
            c.accent_soft
        };
        col = col.child(
            div()
                .id(item.id)
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(10.))
                .py(px(6.))
                .when(item.separated, |el| {
                    el.mt(px(4.)).pt(px(8.)).border_t_1().border_color(c.border)
                })
                .text_size(px(12.5))
                .text_color(fg)
                .cursor_pointer()
                .hover(|el| el.bg(hover_bg))
                .on_mouse_down(MouseButton::Left, move |_, window, cx| {
                    on_click(window, cx);
                    on_dismiss(window, cx);
                })
                .child(item.icon.size(px(13.)).color(icon_fg))
                .child(item.label.clone()),
        );
    }

    deferred(anchored().position(anchor).snap_to_window().child(col))
        .with_priority(gpui_kit::base::POPUP_PRIORITY)
        .into_any_element()
}
