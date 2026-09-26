//! `dtb-ke-debugger` — the developer-only minidump viewer. Not shipped, not built by CI.

mod tokio_bridge;
mod ui;

use std::path::PathBuf;

use dtb_ke_ui::filesystem::FilesystemHelper;
use dtb_ke_ui::settings::Settings;
use dtb_ke_ui::theme::{Appearance, Theme};
use dtb_ke_ui::i18n;
use gpui_kit::{KeyBinding, Menu, MenuItem};

use ui::{AddSymbols, OpenDump, Quit, ShowLogs, ToggleRegisters, ToggleSidebar, ToggleSource};

/// Where this process keeps its data root: a **temporary directory of its own**, so the debugger never writes logs,
/// settings or backups into the regular app's folders. Everything the shared `FilesystemHelper` resolves (the log
/// folder the logs window reads, `Settings.toml` for the theme toggle) lands there and is deleted on quit. The symbol
/// cache is the one thing that must outlive a run; it lives in the OS cache directory instead (`ui/mod.rs`).
///
/// A `DTB_KE_DATA_DIR` set by the caller wins (an explicit choice), and is then left alone.
fn isolate_data_root() -> Option<PathBuf> {
    if std::env::var_os("DTB_KE_DATA_DIR").is_some() {
        return None;
    }
    sweep_stale_roots();
    let dir = std::env::temp_dir().join(format!("{ROOT_PREFIX}{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    // SAFETY: called first thing in `main`, before any other thread exists or anything reads the environment.
    unsafe { std::env::set_var("DTB_KE_DATA_DIR", &dir) };
    Some(dir)
}

const ROOT_PREFIX: &str = "dtb-ke-debugger-";

/// A run that was killed (Ctrl-C under `cargo run`, a crash) leaves its root behind; drop the ones over a day old.
fn sweep_stale_roots() {
    let day = std::time::Duration::from_secs(24 * 60 * 60);
    for entry in std::fs::read_dir(std::env::temp_dir())
        .into_iter()
        .flatten()
        .flatten()
    {
        let name = entry.file_name();
        let stale = name.to_string_lossy().starts_with(ROOT_PREFIX)
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .ok()
                .and_then(|t| t.elapsed().ok())
                .is_some_and(|age| age > day);
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn main() {
    let data_root = isolate_data_root();
    dtb_ke_log::DTBKELogger::new(FilesystemHelper::instance().get_log_file())
        .expect("log file creation must succeed")
        .register();

    // `dtb-ke-debugger [dump.dmp] [debug files or directories…]`; `DTB_KE_SYMBOLS` (path-list) adds more.
    let mut args = std::env::args_os().skip(1).map(PathBuf::from);
    let dump = args.next();
    let mut symbol_paths: Vec<PathBuf> = args.collect();
    if let Some(v) = std::env::var_os("DTB_KE_SYMBOLS") {
        symbol_paths.extend(std::env::split_paths(&v));
    }

    let app = gpui_kit::platform::application();
    app.run(move |cx| {
        // Delete the temporary data root (the logs in it included) when the app quits.
        if let Some(root) = data_root.clone() {
            cx.on_app_quit(move |_| {
                let root = root.clone();
                async move {
                    let _ = std::fs::remove_dir_all(root);
                }
            })
            .detach();
        }
        gpui_kit::base::init(cx);
        tokio_bridge::init(cx);

        Settings::init(cx);
        let settings = Settings::global(cx);
        cx.set_reduce_motion(settings.reduce_motion);
        dtb_ke_log::set_level(settings.log_level.to_filter());
        i18n::Locale::install(cx);
        Theme::install(settings.theme_mode, Appearance::from(cx.window_appearance()), cx);

        let secondary = if cfg!(target_os = "macos") { "cmd" } else { "ctrl" };
        cx.bind_keys([
            KeyBinding::new(&format!("{secondary}-o"), OpenDump, None),
            KeyBinding::new(&format!("{secondary}-shift-o"), AddSymbols, None),
            KeyBinding::new(&format!("{secondary}-q"), Quit, None),
            KeyBinding::new(&format!("{secondary}-l"), ShowLogs, None),
            KeyBinding::new(&format!("{secondary}-alt-s"), ToggleSidebar, None),
            KeyBinding::new(&format!("{secondary}-alt-c"), ToggleSource, None),
            KeyBinding::new(&format!("{secondary}-alt-r"), ToggleRegisters, None),
        ]);
        cx.on_action(|_: &Quit, cx| cx.quit());
        cx.set_menus(vec![
            Menu::new("DTB KE Debugger").items([
                MenuItem::action("Logs", ShowLogs),
                MenuItem::separator(),
                MenuItem::action("Quit", Quit),
            ]),
            Menu::new("File").items([
                MenuItem::action("Open …", OpenDump),
                MenuItem::action("Add debug files …", AddSymbols),
            ]),
            Menu::new("View").items([
                MenuItem::action("Toggle sidebar", ToggleSidebar),
                MenuItem::action("Toggle source pane", ToggleSource),
                MenuItem::action("Toggle register pane", ToggleRegisters),
            ]),
        ]);

        ui::open(cx, dump, symbol_paths);
        cx.activate(true);
    });
}
