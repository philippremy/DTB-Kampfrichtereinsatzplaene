//! `dtb-ke-debugger` — the developer's crash-report viewer (`.dtbkedmp`). Packaged locally
//! (`cargo dtb-ke-bundle bundle --product debugger`), not built by CI, no updater.

// minidump-processor's nested async futures overflow the default (128) auto-trait (`Send`) check.
#![recursion_limit = "256"]

mod app_menu;
mod build;
mod code;
mod discover;
mod dyld;
mod git;
mod highlight;
mod process;
mod identity;
mod open_files;
mod progress;
mod quality;
mod rawdump;
mod remote;
mod resolve;
mod server_settings;
mod source;
mod sources;
mod symbolize;
#[cfg(test)]
mod tests;
mod tokio_bridge;
mod ui;

use std::path::PathBuf;

use dtb_ke_ui::about::{self, Identity};
use dtb_ke_ui::filesystem::FilesystemHelper;
use dtb_ke_ui::i18n;
use dtb_ke_ui::settings::Settings;
use dtb_ke_ui::theme::{Appearance, Theme};

/// The product name — window titles, the About window, the menu bar.
const NAME: &str = "DTB Kampfrichtereinsatzpläne Debugger";
/// Reverse-DNS identifier (ASCII; matches `dtb-ke-bundle`'s `meta::DEBUGGER`).
const IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzplaene.Debugger";

/// The flat icon for the About window on Windows / Linux (macOS reads the live bundle icon). Embedded only once
/// `cargo dtb-ke-bundle icons --product debugger` has generated it.
#[cfg(has_app_icon)]
const ICON_PNG: Option<&[u8]> = Some(include_bytes!(
    "../../../assets/icons/debugger/generated/AppIcon.png"
));
#[cfg(not(has_app_icon))]
const ICON_PNG: Option<&[u8]> = None;

/// Whether `path` is a crash report this tool opens: a `.dtbkedmp` (case-insensitive).
fn is_dump(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(dtb_ke_crash::DUMP_EXTENSION))
}

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

    // `dtb-ke-debugger [dump.dtbkedmp] [debug files or directories…]` — a file association launches it with just
    // the dump. The dump is the first argument with the right extension; every other argument is a symbol
    // source. `DTB_KE_SYMBOLS` (path-list) adds more.
    let mut dump = None;
    let mut symbol_paths = Vec::new();
    for arg in std::env::args_os().skip(1).map(PathBuf::from) {
        if dump.is_none() && is_dump(&arg) {
            dump = Some(arg);
        } else {
            symbol_paths.push(arg);
        }
    }
    if let Some(v) = std::env::var_os("DTB_KE_SYMBOLS") {
        symbol_paths.extend(std::env::split_paths(&v));
    }

    let app = gpui_kit::platform::application();
    // A Dock click after the last window was closed (macOS keeps the app alive), and a file opened from Finder.
    app.on_reopen(|cx| ui::open(cx, None, Vec::new()));
    app.on_open_urls(open_files::handle_open_urls);
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
        Theme::install(
            settings.theme_mode,
            Appearance::from(cx.window_appearance()),
            cx,
        );
        about::install_identity(
            cx,
            Identity {
                name: NAME,
                // The long name would wrap at the app's headline size.
                name_size: 18.,
                identifier: IDENTIFIER,
                icon_png: ICON_PNG,
            },
        );

        app_menu::install(cx);

        ui::open(cx, dump, symbol_paths);
        open_files::ready(cx);
        cx.activate(true);
    });
}
