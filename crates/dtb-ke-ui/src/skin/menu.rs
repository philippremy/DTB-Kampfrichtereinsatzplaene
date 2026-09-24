//! Installing the application menu bar — the platform seam.
//!
//! The menu *model* is platform-agnostic (see [`crate::menu`]); this module owns
//! how it reaches the user:
//!
//! * **macOS** — [`App::set_menus`] builds a real `NSMenu` in the system menu
//!   bar, deriving each item's key equivalent from the active keymap.
//! * **iPadOS** — gpui_ios builds the system menu bar with `UIMenuBuilder` from the same model
//!   (native, like macOS; item enablement comes from `validateCommand:`).
//! * **Windows / Linux** — gpui only *stores* the model on these platforms; it
//!   does not draw a bar. Phases 7/8 add an in-app menu bar that renders the
//!   same [`crate::menu::build`] output. We still hand gpui the model so the
//!   Windows jump list / dock menu can reuse it later.

use gpui_kit::App;

use crate::i18n::Locale;
use crate::menu::{self, MenuState};
use crate::theme::{ActiveTheme, MenuStyle};

/// (Re)install the native menu bar for `state`. Cheap enough to call whenever the
/// state that decides labels / enabled items changes. A no-op visually on
/// Windows / Linux (gpui stores but doesn't render it there) — see [`in_app`].
pub fn install(state: MenuState, cx: &mut App) {
    let menus = menu::build(state, cx.global::<Locale>());
    cx.set_menus(menus);
}

/// Whether the app must draw its own menu affordance (Windows strip / Linux ☰).
/// macOS uses the system bar installed by [`install`].
pub fn in_app(cx: &App) -> bool {
    cx.theme().skin.menu_bar != MenuStyle::Native
}

/// Whether the platform has app-level window commands (hide, hide others, quit, minimize, full
/// screen). iPadOS windows are managed by the system (Stage Manager), so those menu items would do
/// nothing there.
pub const fn has_window_commands() -> bool {
    !cfg!(target_os = "ios")
}

/// Whether the platform has a file manager to reveal folders and files in (the "open log folder"
/// and "show database" items). iPadOS apps live in a sandbox the user cannot browse from here.
pub const fn has_file_manager() -> bool {
    !cfg!(target_os = "ios")
}
