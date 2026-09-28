// Do not show a console on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

use log::{debug, error, info};

use dtb_ke_ui::menu::MenuState;
use dtb_ke_ui::settings::Settings;
use dtb_ke_ui::theme::{Appearance, Theme};
use dtb_ke_ui::{
    about, actions, app, build_info, components, debug, feedback_window, filesystem, i18n, keymap,
    logs_window, open_files, preview, save, settings, settings_window, sheet, skin, stress,
};
// Only referenced by the self-relaunch dispatch below, which is `#[cfg]`'d out on iOS (no separate
// process to relaunch into — see `dtb-ke-crash`'s module doc comment).
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
use dtb_ke_ui::crash_report;

fn main() {
    // A re-launched instance in crash-reporter mode. This MUST run before logging + crash-handler
    // install so that a fault *in the reporter* can never spawn a second reporter.
    //
    // Every platform self-relaunches (see `dtb-ke-crash`'s module doc comment): the crashed process
    // `execve`/`CreateProcessW`s this same binary with `dtb_ke_crash::RELAUNCH_ARG` as its first
    // argument, so recognising it is just checking that flag — no separate helper process exists any
    // more for a parent-process check to make sense against.
    #[cfg(target_os = "linux")]
    if std::env::args().nth(1).as_deref() == Some(dtb_ke_crash::RELAUNCH_ARG) {
        std::process::exit(crash_report::run_linux_capture());
    }
    #[cfg(target_os = "macos")]
    if std::env::args().nth(1).as_deref() == Some(dtb_ke_crash::RELAUNCH_ARG) {
        std::process::exit(crash_report::run_macos_capture());
    }
    #[cfg(target_os = "windows")]
    if std::env::args().nth(1).as_deref() == Some(dtb_ke_crash::RELAUNCH_ARG) {
        std::process::exit(crash_report::run_windows_capture());
    }

    // Developer options that are only read from the environment (see `debug`) must be exported before
    // the logger or any other thread exists.
    settings::Settings::load().debug.apply_startup_env();

    // Initialize logging. Records go straight to the session file (no buffer,
    // no flush thread); the level filter is retuned from `Settings` below and
    // can change again at runtime via the settings window.
    dtb_ke_log::DTBKELogger::new(filesystem::FilesystemHelper::instance().get_log_file())
        .expect("Log file creation must succeed")
        .register();

    info!(
        "Logger initialized, logging to {}",
        filesystem::FilesystemHelper::instance()
            .get_log_file()
            .display()
    );

    // Housekeeping: a session log accumulates on every launch and nothing
    // else ever removes an old one, so they grow without the user noticing.
    match filesystem::FilesystemHelper::instance().gc_old_logs() {
        0 => debug!("log gc: nothing older than 7 days"),
        removed => info!("log gc: removed {removed} log file(s) older than 7 days"),
    }

    save::remove_stale_staging();

    // Crash capture, before anything else can fault. On a hardware fault or a panic an out-of-process
    // helper writes a minidump (`.dtbkedmp`) under `logs/crashes/` for offline symbolisation (the shipped
    // binary is stripped — see `dtb-ke-crash`). iOS cannot spawn a helper: the handler runs in-process
    // and only writes a compact snapshot, which the *next* launch turns into the same `.dtbkedmp` and offers
    // to send (`crash_report::offer_pending`). A panic's message and backtrace also reach the session
    // log (hook installed first, so the crash handler's hook chains to it).
    #[cfg(target_os = "ios")]
    {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            error!("panic: {info}\n{}", std::backtrace::Backtrace::force_capture());
            previous(info);
        }));
    }
    if let Err(err) = dtb_ke_crash::library::install(dtb_ke_crash::library::Config {
        dump_dir: filesystem::FilesystemHelper::instance()
            .get_log_dir()
            .join("crashes"),
        app_slug: "DTB-KE".to_string(),
        build_info: build_info::stream_text(),
    }) {
        error!("Failed to install global crash handler: {err}");
    } else {
        info!("Installed global crash handler")
    }

    info!(
        "DTB-Kampfrichtereinsatzpläne {} ({})",
        build_info::APP_VERSION,
        build_info::PROFILE
    );

    let app = gpui_kit::platform::application();
    // macOS: the app keeps running when the main window is closed; clicking the
    // dock icon re-opens it (or re-focuses it if it's still around).
    app.on_reopen(app::open_main_window);
    // Opening a `.dtbke` file from the OS. Real (and possibly the only
    // signal we get) on macOS; a harmless no-op registration on Windows/Linux
    // — see `open_files`'s module doc for why those instead rely on the
    // command-line argument, handled by `open_files::ready` below.
    app.on_open_urls(open_files::handle_open_urls);
    app.run(|cx| {
        // `gpui-base` global infrastructure (input engine, popovers, …), via
        // gpui-kit. It installs a default `gpui_kit::base::Theme`; our
        // `Theme::install` overrides its colour tokens right after.
        gpui_kit::base::init(cx);
        debug!("gpui-kit base initialised");

        // The About window's app icon: on macOS, fetched natively (live
        // appearance-adaptive on Tahoe+) instead of an embedded copy; see
        // `skin::app_icon`. No-op watch on Windows/Linux.
        about::install_icon(cx);

        // Persisted user settings, installed as a global so the settings window
        // and the editor layer read the same source of truth.
        Settings::init(cx);
        let settings = Settings::global(cx);
        cx.set_reduce_motion(settings.reduce_motion);
        dtb_ke_log::set_level(settings.log_level.to_filter());
        debug!(
            "settings loaded — theme {:?}, log level {:?}, autosave {:?}, reduce_motion {}",
            settings.theme_mode, settings.log_level, settings.autosave, settings.reduce_motion
        );

        // Resolves Settings.locale (or the OS's own preference list, or the
        // hardcoded German default) into the active string catalog. Must run
        // before anything below that renders translated text.
        i18n::Locale::install(cx);

        // Persisted light/dark preference + the OS's current appearance. The
        // window then keeps the OS side in step via `observe_window_appearance`
        // (see `AppShell`).
        Theme::install(
            settings.theme_mode,
            Appearance::from(cx.window_appearance()),
            cx,
        );
        // macOS only: re-tint live when the user changes the OS accent
        // colour (System Settings → Appearance). No-op elsewhere.
        skin::accent::watch_system_accent(cx);

        // Menu commands: key bindings (defaults + the user's overrides), the
        // window-independent action handlers, and the menu bar itself. `AppShell`
        // re-installs the bar whenever the enabled/label state changes.
        keymap::install(&settings.keybindings, cx);
        actions::register_global_handlers(cx);
        skin::menu::install(MenuState::default(), cx);
        sheet::init(cx);
        cx.set_global(preview::PreviewPane::default());

        if std::env::var_os("DTB_KE_GALLERY").is_some() {
            debug!("DTB_KE_GALLERY set — opening the component gallery");
            components::gallery::open_gallery_window(cx);
        } else {
            app::open_main_window(cx);
        }

        // Opens a `.dtbke` file passed on the command line (Windows/Linux
        // file-association launch) and hands `on_open_urls` a way to reach
        // `cx` for anything it sees from here on (macOS).
        open_files::ready(cx);

        // Debug aids: open a secondary window straight away.
        if let Some(v) = std::env::var_os("DTB_KE_ABOUT") {
            about::open_named(cx, &v.to_string_lossy());
        }
        if let Ok(action) = std::env::var("DTB_KE_ACTION") {
            app::debug_dispatch_file_action(&action, cx);
        }
        if std::env::var_os("DTB_KE_PREVIEW").is_some() {
            app::debug_show_preview(cx);
        }
        if std::env::var_os("DTB_KE_SETTINGS").is_some() {
            settings_window::open(cx);
        }
        if std::env::var_os("DTB_KE_LOGS").is_some() {
            logs_window::open(cx);
        }
        match std::env::var("DTB_KE_FEEDBACK").as_deref() {
            Ok("bug") => feedback_window::open_bug(cx),
            Ok("feature") => feedback_window::open_feature(cx),
            _ => {}
        }

        // Crash-handler smoke test: `DTB_KE_CRASH_TEST=segv|panic|bus|borrow|abort|overflow[,thread]`
        // faults ~2 s after launch (see `debug::crash`).
        debug::crash::from_env(cx);

        if std::env::var_os("DTB_KE_STRESS").is_some() {
            stress::start(cx);
        }

        cx.activate(true);
    });
}
