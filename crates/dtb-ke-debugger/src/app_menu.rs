//! The application menu, its key bindings and the window-independent command handlers.
//!
//! macOS gets a native menu bar (`cx.set_menus`); Windows and Linux draw the same model with the app's in-app
//! `MenuBar` (see `DebuggerWindow::render`). The debugger's UI is English, so the labels are too — only the
//! About window follows the user's language, as it is shared with the main app.

use dtb_ke_debugger::NAME;
use dtb_ke_ui::actions::REPOSITORY_URL;
use dtb_ke_ui::skin::menu::has_window_commands;
use gpui_kit::{App, KeyBinding, Menu, MenuItem};

use crate::ui::{
    About, AddSymbols, CloseWindow, HideApp, HideOthers, Minimize, OpenDump, OpenRepository, Quit,
    ShowLogs, ToggleFullscreen, ToggleRegisters, ToggleSidebar, ToggleSource, Zoom,
};

/// Every binding: (action name, keystroke, constructor). `secondary` is ⌘ on macOS, Ctrl elsewhere.
fn bindings() -> Vec<(&'static str, String, Box<dyn Fn() -> KeyBinding>)> {
    let secondary = if cfg!(target_os = "macos") {
        "cmd"
    } else {
        "ctrl"
    };
    macro_rules! b {
        ($name:literal, $keys:expr, $action:expr) => {{
            let keys: String = $keys;
            let bound = keys.clone();
            (
                $name,
                keys,
                Box::new(move || KeyBinding::new(&bound, $action, None))
                    as Box<dyn Fn() -> KeyBinding>,
            )
        }};
    }
    let mut all = vec![
        b!("debugger::OpenDump", format!("{secondary}-o"), OpenDump),
        b!(
            "debugger::AddSymbols",
            format!("{secondary}-shift-o"),
            AddSymbols
        ),
        b!(
            "debugger::CloseWindow",
            format!("{secondary}-w"),
            CloseWindow
        ),
        b!("debugger::Quit", format!("{secondary}-q"), Quit),
        b!("debugger::ShowLogs", format!("{secondary}-l"), ShowLogs),
        b!(
            "debugger::ToggleSidebar",
            format!("{secondary}-alt-s"),
            ToggleSidebar
        ),
        b!(
            "debugger::ToggleSource",
            format!("{secondary}-alt-c"),
            ToggleSource
        ),
        b!(
            "debugger::ToggleRegisters",
            format!("{secondary}-alt-r"),
            ToggleRegisters
        ),
    ];
    if cfg!(target_os = "macos") {
        all.push(b!("debugger::Minimize", "cmd-m".to_owned(), Minimize));
        all.push(b!("debugger::HideApp", "cmd-h".to_owned(), HideApp));
        all.push(b!(
            "debugger::HideOthers",
            "cmd-alt-h".to_owned(),
            HideOthers
        ));
        all.push(b!(
            "debugger::ToggleFullscreen",
            "ctrl-cmd-f".to_owned(),
            ToggleFullscreen
        ));
    } else {
        all.push(b!(
            "debugger::ToggleFullscreen",
            "f11".to_owned(),
            ToggleFullscreen
        ));
    }
    all
}

/// Shortcut hints for the in-app menu (`MenuBar::set_hints`).
pub fn hints() -> Vec<(&'static str, String)> {
    bindings()
        .into_iter()
        .map(|(name, keys, _)| (name, keys))
        .collect()
}

/// The menu model.
pub fn build() -> Vec<Menu> {
    let mut app = vec![MenuItem::action(format!("About {NAME}"), About)];
    #[cfg(target_os = "macos")]
    {
        app.push(MenuItem::separator());
        app.push(MenuItem::os_submenu(
            "Services",
            gpui_kit::SystemMenuType::Services,
        ));
    }
    if has_window_commands() {
        app.extend([
            MenuItem::separator(),
            MenuItem::action(format!("Hide {NAME}"), HideApp),
            MenuItem::action("Hide Others", HideOthers),
        ]);
    }
    app.extend([
        MenuItem::separator(),
        MenuItem::action(format!("Quit {NAME}"), Quit),
    ]);

    let mut view = vec![
        MenuItem::action("Toggle Sidebar", ToggleSidebar),
        MenuItem::action("Toggle Source Pane", ToggleSource),
        MenuItem::action("Toggle Register Pane", ToggleRegisters),
    ];
    if has_window_commands() {
        view.extend([
            MenuItem::separator(),
            MenuItem::action("Toggle Full Screen", ToggleFullscreen),
        ]);
    }

    let mut window = vec![MenuItem::action("Minimize", Minimize)];
    if cfg!(target_os = "macos") {
        window.push(MenuItem::action("Zoom", Zoom));
    }

    vec![
        Menu::new(NAME).items(app),
        Menu::new("File").items([
            MenuItem::action("Open …", OpenDump),
            MenuItem::action("Add Debug Files …", AddSymbols),
            MenuItem::separator(),
            MenuItem::action("Close Window", CloseWindow),
        ]),
        Menu::new("View").items(view),
        Menu::new("Window").items(window),
        Menu::new("Help").items([
            MenuItem::action("Show Logs …", ShowLogs),
            MenuItem::separator(),
            MenuItem::action("Open Repository", OpenRepository),
        ]),
    ]
}

/// Bind the keys, register the window-independent handlers and install the menu bar.
pub fn install(cx: &mut App) {
    cx.bind_keys(bindings().into_iter().map(|(_, _, make)| make()));

    cx.on_action(|_: &Quit, cx| {
        log::info!("quit requested");
        cx.quit()
    });
    cx.on_action(|_: &HideApp, cx| cx.hide());
    cx.on_action(|_: &HideOthers, cx| cx.hide_other_apps());
    cx.on_action(|_: &About, cx| dtb_ke_ui::about::open(cx));
    cx.on_action(|_: &ShowLogs, cx| dtb_ke_ui::logs_window::open(cx));
    cx.on_action(|_: &OpenRepository, cx| cx.open_url(REPOSITORY_URL));

    cx.set_menus(build());
}
