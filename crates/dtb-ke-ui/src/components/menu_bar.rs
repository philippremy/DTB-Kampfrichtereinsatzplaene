//! In-app application menu affordance.
//!
//! gpui renders a native menu bar only on macOS. This entity draws the exact
//! same model [`crate::menu::build`] produces, in one of two layouts chosen by
//! the skin's [`MenuStyle`](crate::theme::MenuStyle):
//!
//! * **`InApp`** (Windows) — a strip of top-level names, each a dropdown; hover
//!   switches menus while one is open.
//! * **`Hamburger`** (Linux) — one `☰` button opening a single popover with
//!   every menu as a labelled section.
//!
//! Clicking an enabled item dispatches its action into the focused window — the
//! same path the native menu uses.

use std::time::Duration;

use gpui_kit::{
    Action, Animation, AnimationExt, Context, Div, InteractiveElement, IntoElement, Menu,
    MouseButton, OwnedMenu, OwnedMenuItem, ParentElement, Pixels, Render, Stateful,
    StatefulInteractiveElement, Styled, Window, deferred, div, ease_out_quint,
    prelude::FluentBuilder, px,
};

use crate::components::button::Button;
use crate::components::icon::Icon;
use crate::components::kbd;
use crate::keymap::{self, Overrides};
use crate::settings::Settings;
use crate::theme::{ActiveTheme, MenuStyle, PaletteColors};

pub struct MenuBar {
    menus: Vec<OwnedMenu>,
    /// Strip mode: index of the open top-level menu. Hamburger mode: `Some(0)`
    /// means the popover is showing.
    open: Option<usize>,
    /// Shortcut hints for actions the app's keymap (`keymap::defaults`) doesn't know — another binary linking
    /// this crate (the debugger) binds its own keys. `(action name, keystroke)`.
    hints: Vec<(&'static str, String)>,
}

impl MenuBar {
    pub fn new() -> Self {
        Self {
            menus: Vec::new(),
            open: None,
            hints: Vec::new(),
        }
    }

    /// Shortcut hints for actions outside `keymap::defaults` (see the field).
    pub fn set_hints(&mut self, hints: Vec<(&'static str, String)>) {
        self.hints = hints;
    }

    /// Replace the menu model (called from `AppShell` whenever the menu state
    /// changes — same trigger as `cx.set_menus` on macOS).
    pub fn set_menus(&mut self, menus: Vec<Menu>, cx: &mut Context<Self>) {
        self.menus = menus.into_iter().map(Menu::owned).collect();
        if self.open.is_some_and(|i| i >= self.menus.len()) {
            self.open = None;
        }
        cx.notify();
    }

    fn toggle(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open = (self.open != Some(index)).then_some(index);
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.open.take().is_some() {
            cx.notify();
        }
    }

    fn activate(&mut self, action: &dyn Action, window: &mut Window, cx: &mut Context<Self>) {
        self.open = None;
        cx.notify();
        window.dispatch_action(action.boxed_clone(), cx);
    }

    /// One command row. `None` for separators / submenus (submenus don't occur
    /// in our model). `key` must be unique among siblings. `overrides` is the
    /// user's key-binding map — the shortcut hint on the right is looked up from
    /// it (gpui paints one automatically only on the native macOS menu).
    fn item_row(
        &self,
        key: u64,
        item: &OwnedMenuItem,
        c: &PaletteColors,
        overrides: &Overrides,
        cx: &mut Context<Self>,
    ) -> Option<Stateful<Div>> {
        match item {
            OwnedMenuItem::Separator => None,
            OwnedMenuItem::Submenu(_) | OwnedMenuItem::SystemMenu(_) => None,
            OwnedMenuItem::Action {
                name,
                action,
                disabled,
                ..
            } => {
                let disabled = *disabled;
                let hint = keymap::effective_keystroke(action.name(), overrides)
                    .or_else(|| {
                        self.hints.iter().find(|(name, _)| *name == action.name()).map(|(_, k)| k.clone())
                    })
                    .map(|k| kbd::plain(&k, cx))
                    .filter(|h| !h.is_empty());
                let action = action.boxed_clone();
                Some(
                    div()
                        // `.hover()` only applies to an element with an id.
                        .id(("menu-item", key))
                        .flex()
                        .items_center()
                        .gap(px(24.))
                        .px(px(12.))
                        .py(px(5.))
                        .text_size(px(12.5))
                        .when(disabled, |el| {
                            el.text_color(c.muted_foreground).opacity(0.55)
                        })
                        .when(!disabled, |el| {
                            el.cursor_pointer()
                                .hover(|el| el.bg(c.accent_soft).text_color(c.primary))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        this.activate(action.as_ref(), window, cx)
                                    }),
                                )
                        })
                        .child(div().flex_1().child(name.clone()))
                        .when_some(hint, |el, hint| {
                            el.child(
                                div()
                                    .flex_none()
                                    .text_size(px(11.))
                                    .text_color(c.muted_foreground)
                                    .child(hint),
                            )
                        }),
                )
            }
        }
    }

    /// The floating panel body: anchored under the trigger. `max_h`, when
    /// given, caps the height and scrolls past that so a long menu is always
    /// fully reachable — the hamburger mega-panel (Linux) is left uncapped
    /// instead, sized to its content.
    fn panel(
        &self,
        id: &'static str,
        max_h: Option<Pixels>,
        c: &PaletteColors,
        radius: Pixels,
    ) -> Stateful<Div> {
        div()
            .id(id)
            .absolute()
            .top_full()
            .min_w(px(220.))
            .when_some(max_h, |el, h| el.max_h(h.max(px(180.))).overflow_y_scroll())
            .flex()
            .flex_col()
            .py(px(4.))
            .rounded(radius)
            .border_1()
            .border_color(c.border)
            .bg(c.surface)
            .text_color(c.foreground)
            .shadow_lg()
            .occlude()
    }

    fn separator(c: &PaletteColors) -> Div {
        div().my(px(4.)).mx(px(8.)).h(px(1.)).bg(c.border)
    }

    /// The open animation for a panel: a short fade + 4px settle, scaled by the
    /// skin's motion factor (and skipped entirely under reduced motion).
    fn appear<E: IntoElement + Styled + 'static>(
        el: E,
        id: &'static str,
        cx: &Context<Self>,
    ) -> impl IntoElement {
        let motion = cx.theme().skin.motion(Duration::from_millis(110));
        el.with_animation(
            id,
            Animation::new(motion).with_easing(ease_out_quint()),
            |el, t| el.opacity(t).mt(px(3.) + px(4.) * (1.0 - t)),
        )
    }

    /// A single top-level menu's dropdown (strip mode).
    fn dropdown(&self, menu_index: usize, max_h: Pixels, cx: &mut Context<Self>) -> Stateful<Div> {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_px();
        let overrides = Settings::global(cx).keybindings;
        let items: Vec<_> = self.menus[menu_index].items.clone();

        let mut col = self
            .panel("menu-dropdown", Some(max_h), &c, radius)
            .left_0();
        // Coalesce separators: an item that renders as `None` (a `SystemMenu` /
        // submenu — e.g. macOS "Dienste") would otherwise leave the separators on
        // both sides of it visible as a double rule. Only emit a separator once a
        // real row precedes it and a real row follows it.
        let mut sep_pending = false;
        let mut emitted_row = false;
        for (ix, item) in items.iter().enumerate() {
            let key = (menu_index * 64 + ix) as u64;
            match item {
                OwnedMenuItem::Separator => sep_pending = emitted_row,
                other => {
                    if let Some(row) = self.item_row(key, other, &c, &overrides, cx) {
                        if sep_pending {
                            col = col.child(Self::separator(&c));
                            sep_pending = false;
                        }
                        col = col.child(row);
                        emitted_row = true;
                    }
                }
            }
        }
        col
    }

    /// One popover holding every menu as a labelled section (hamburger mode).
    /// Uncapped height — sized to its content, not to the viewport.
    fn mega_panel(&self, cx: &mut Context<Self>) -> Stateful<Div> {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_px();
        let overrides = Settings::global(cx).keybindings;
        let menus: Vec<OwnedMenu> = self.menus.clone();

        let mut col = self
            .panel("menu-mega-panel", Some(px(420.)), &c, radius)
            .left_0();
        for (mi, menu) in menus.iter().enumerate() {
            if mi > 0 {
                col = col.child(Self::separator(&c));
            }
            col = col.child(
                div()
                    .px(px(12.))
                    .pt(px(4.))
                    .pb(px(2.))
                    .text_size(px(9.5))
                    .text_color(c.muted_foreground)
                    .child(menu.name.to_string().to_uppercase()),
            );
            for (ix, item) in menu.items.iter().enumerate() {
                let key = (mi * 64 + ix) as u64;
                match item {
                    OwnedMenuItem::Separator => {}
                    other => {
                        if let Some(row) = self.item_row(key, other, &c, &overrides, cx) {
                            col = col.child(row);
                        }
                    }
                }
            }
        }
        col
    }

    fn render_strip(&mut self, window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.open;
        let count = self.menus.len();
        let max_h = window.viewport_size().height - px(112.);

        div()
            .id("menu-bar")
            .flex()
            .items_center()
            .h(px(28.))
            .gap(px(1.))
            .px(px(4.))
            .when(open.is_some(), |el| {
                el.on_mouse_down_out(cx.listener(|this, _, _window, cx| this.close(cx)))
            })
            .children((0..count).map(|i| {
                let is_open = open == Some(i);
                let name = self.menus[i].name.clone();
                div()
                    .relative()
                    .child(
                        div()
                            .id(("menu-title", i))
                            .px(px(8.))
                            .py(px(3.))
                            .rounded(radius)
                            .text_size(px(12.5))
                            .cursor_pointer()
                            .when(is_open, |el| el.bg(c.accent_soft).text_color(c.primary))
                            .when(!is_open, |el| {
                                el.hover(|el| {
                                    el.bg(gpui_kit::Hsla {
                                        a: 0.10,
                                        ..c.foreground
                                    })
                                })
                            })
                            .on_click(cx.listener(move |this, _, _window, cx| this.toggle(i, cx)))
                            .on_hover(cx.listener(move |this, hovered: &bool, _window, cx| {
                                if *hovered && this.open.is_some() && this.open != Some(i) {
                                    this.open = Some(i);
                                    cx.notify();
                                }
                            }))
                            .child(name),
                    )
                    .when(is_open, |el| {
                        let dd =
                            Self::appear(self.dropdown(i, max_h, cx), "menu-dropdown-anim", cx);
                        el.child(deferred(dd))
                    })
            }))
    }

    fn render_hamburger(&mut self, _window: &Window, cx: &mut Context<Self>) -> impl IntoElement {
        let is_open = self.open.is_some();

        div()
            .id("menu-hamburger")
            .relative()
            .flex()
            .items_center()
            .px(px(2.))
            .when(is_open, |el| {
                el.on_mouse_down_out(cx.listener(|this, _, _window, cx| this.close(cx)))
            })
            .child(
                Button::icon("menu-hamburger-btn", Icon::Menu)
                    .on_click(cx.listener(|this, _, _window, cx| this.toggle(0, cx))),
            )
            .when(is_open, |el| {
                let panel = Self::appear(self.mega_panel(cx), "menu-mega-anim", cx);
                el.child(deferred(panel))
            })
    }
}

impl Render for MenuBar {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if cx.theme().skin.menu_bar == MenuStyle::Hamburger {
            self.render_hamburger(window, cx).into_any_element()
        } else {
            self.render_strip(window, cx).into_any_element()
        }
    }
}
