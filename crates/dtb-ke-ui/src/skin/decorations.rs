//! Client-side window decorations — the platform seam for skins whose
//! `window_decorations` is `Client` (Linux). On `Server` skins (macOS, Windows)
//! every function here is a pass-through / no-op.
//!
//! Adapted from Zed's `workspace::client_side_decorations`: a transparent
//! backdrop that reserves room for a drop shadow, an inner surface with rounded
//! corners + hairline border, and edge / corner resize hit-testing that drives
//! `window.start_window_resize`.

use gpui::{
    App, Bounds, CursorStyle, Decorations, Global, HitboxBehavior, InteractiveElement, IntoElement,
    MouseButton, ParentElement, Pixels, Point, ResizeEdge, Size, Styled, Tiling, Window,
    WindowDecorations, canvas, div, point, prelude::FluentBuilder, px, size, transparent_black,
};

use crate::theme::{ActiveTheme, Theme, WindowDecorations as SkinDecorations};

/// Thickness of the drop-shadow gutter around a CSD window (also the resize
/// grab margin).
const SHADOW: Pixels = px(10.0);
const BORDER: Pixels = px(1.0);

/// The [`WindowDecorations`] to request for this skin.
pub fn requested(theme: &Theme) -> WindowDecorations {
    match theme.skin.window_decorations {
        SkinDecorations::Client => WindowDecorations::Client,
        SkinDecorations::Server => WindowDecorations::Server,
    }
}

/// Whether the *live* window is running with client-side decorations.
pub fn is_client(window: &Window) -> bool {
    matches!(window.window_decorations(), Decorations::Client { .. })
}

struct GlobalResizeEdge(ResizeEdge);
impl Global for GlobalResizeEdge {}

/// Wrap the window's root element in the CSD chrome. A pass-through when the
/// window uses server-side decorations.
pub fn apply_frame(inner: impl IntoElement, window: &mut Window, cx: &mut App) -> impl IntoElement {
    let decorations = window.window_decorations();
    let tiling = match decorations {
        Decorations::Client { tiling } => tiling,
        Decorations::Server => Tiling::default(),
    };
    match decorations {
        Decorations::Client { .. } => window.set_client_inset(SHADOW),
        Decorations::Server => window.set_client_inset(px(0.0)),
    }

    let is_resizable = window.is_resizable();
    let c = cx.theme().color;
    let corner = cx.theme().skin.radius_lg_px();

    div()
        .id("window-backdrop")
        .size_full()
        .bg(transparent_black())
        .map(|el| match decorations {
            Decorations::Server => el,
            Decorations::Client { .. } => el
                .when(!tiling.top, |el| el.pt(SHADOW))
                .when(!tiling.bottom, |el| el.pb(SHADOW))
                .when(!tiling.left, |el| el.pl(SHADOW))
                .when(!tiling.right, |el| el.pr(SHADOW))
                .when(is_resizable, |el| {
                    el.on_mouse_down(MouseButton::Left, move |ev, window, _| {
                        let size = window.window_bounds().get_bounds().size;
                        if let Some(edge) = resize_edge(ev.position, size, tiling) {
                            window.start_window_resize(edge);
                        }
                    })
                }),
        })
        .child(
            div()
                .size_full()
                .cursor(CursorStyle::Arrow)
                .map(|el| match decorations {
                    Decorations::Server => el,
                    Decorations::Client { .. } => el
                        .bg(c.background)
                        .border_color(c.border)
                        .when(!tiling.top, |el| el.border_t(BORDER).rounded_t(corner))
                        .when(!tiling.bottom, |el| el.border_b(BORDER).rounded_b(corner))
                        .when(!tiling.left, |el| el.border_l(BORDER))
                        .when(!tiling.right, |el| el.border_r(BORDER))
                        .when(!tiling.is_tiled(), |el| el.shadow_lg()),
                })
                // Content sits above the resize gutter: stop hover/move here so
                // the edge hit-test only fires in the actual margin.
                .on_mouse_move(|_, _, cx| cx.stop_propagation())
                .child(inner),
        )
        .when(
            matches!(decorations, Decorations::Client { .. }) && is_resizable,
            |el| {
                el.child(
                    canvas(
                        |_, window, _| {
                            window.insert_hitbox(
                                Bounds::new(
                                    point(px(0.0), px(0.0)),
                                    window.window_bounds().get_bounds().size,
                                ),
                                HitboxBehavior::Normal,
                            )
                        },
                        move |_, hitbox, window, cx| {
                            let size = window.window_bounds().get_bounds().size;
                            let Some(edge) = resize_edge(window.mouse_position(), size, tiling)
                            else {
                                return;
                            };
                            cx.set_global(GlobalResizeEdge(edge));
                            window.set_cursor_style(cursor_for(edge), &hitbox);
                        },
                    )
                    .size_full()
                    .absolute(),
                )
            },
        )
}

fn cursor_for(edge: ResizeEdge) -> CursorStyle {
    match edge {
        ResizeEdge::Top | ResizeEdge::Bottom => CursorStyle::ResizeUpDown,
        ResizeEdge::Left | ResizeEdge::Right => CursorStyle::ResizeLeftRight,
        ResizeEdge::TopLeft | ResizeEdge::BottomRight => CursorStyle::ResizeUpLeftDownRight,
        ResizeEdge::TopRight | ResizeEdge::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
    }
}

fn resize_edge(
    pos: Point<Pixels>,
    window_size: Size<Pixels>,
    tiling: Tiling,
) -> Option<ResizeEdge> {
    let inner = Bounds::new(Point::default(), window_size).inset(SHADOW * 1.5);
    if inner.contains(&pos) {
        return None;
    }

    let corner = size(SHADOW * 1.5, SHADOW * 1.5);
    let corners = [
        (Point::new(px(0.), px(0.)), ResizeEdge::TopLeft, !tiling.top),
        (
            Point::new(window_size.width - corner.width, px(0.)),
            ResizeEdge::TopRight,
            !tiling.top,
        ),
        (
            Point::new(px(0.), window_size.height - corner.height),
            ResizeEdge::BottomLeft,
            !tiling.bottom,
        ),
        (
            Point::new(
                window_size.width - corner.width,
                window_size.height - corner.height,
            ),
            ResizeEdge::BottomRight,
            !tiling.bottom,
        ),
    ];
    for (origin, edge, enabled) in corners {
        if enabled && Bounds::new(origin, corner).contains(&pos) {
            return Some(edge);
        }
    }

    if !tiling.top && pos.y < SHADOW {
        Some(ResizeEdge::Top)
    } else if !tiling.bottom && pos.y > window_size.height - SHADOW {
        Some(ResizeEdge::Bottom)
    } else if !tiling.left && pos.x < SHADOW {
        Some(ResizeEdge::Left)
    } else if !tiling.right && pos.x > window_size.width - SHADOW {
        Some(ResizeEdge::Right)
    } else {
        None
    }
}
