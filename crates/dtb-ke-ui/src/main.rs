// The UI layer is still being built out — the backend and theme layer expose
// more API than the current (stub) views consume.
#![allow(dead_code)]
// Do not show a console on Windows
#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]

mod about;
mod actions;
mod app;
mod build_info;
mod components;
mod crash_report;
mod detail;
mod feedback_window;
mod filesystem;
mod i18n;
mod keymap;
mod logs_window;
mod mail;
mod material;
mod menu;
mod model;
mod preview;
mod save;
mod settings;
mod settings_window;
mod sidebar;
mod skin;
mod store;
mod theme;
mod toolbar;
mod trash_window;
mod updater;

use log::{debug, error, info};

use crate::menu::MenuState;
use crate::settings::Settings;
use crate::theme::{Appearance, Theme};

fn main() {
    // A re-launched instance in crash-reporter mode — spawned by the crash
    // helper, which we recognise as our parent process (no flag, no env var).
    // Show the crash dialog, return its verdict as the exit code, and nothing
    // else. This MUST run before logging + crash-handler install so that a fault
    // *in the reporter* can never spawn a second reporter.
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    if crash_report::launched_by_crash_helper() {
        std::process::exit(crash_report::run());
    }

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

    // Crash capture, before anything else can fault. On a hardware fault or a
    // panic an out-of-process helper writes a minidump (`.dmp`) under `logs/
    // crashes/` for offline symbolisation (the shipped binary is stripped — see
    // `dtb-ke-crash`).
    if let Err(err) = dtb_ke_crash::library::install(dtb_ke_crash::library::Config {
        dump_dir: filesystem::FilesystemHelper::instance()
            .get_log_dir()
            .join("crashes"),
        app_slug: "DTB-KE".to_string(),
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

    let app = gpui_platform::application();
    // macOS: the app keeps running when the main window is closed; clicking the
    // dock icon re-opens it (or re-focuses it if it's still around).
    app.on_reopen(app::open_main_window);
    app.run(|cx| {
        // `gpui-base` global infrastructure (input engine, popovers, …). It
        // installs a default `gpui_base::Theme`; our `Theme::install` overrides
        // its colour tokens right after.
        gpui_base::init(cx);
        debug!("gpui-base initialised");

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
        crate::i18n::Locale::install(cx);

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

        if std::env::var_os("DTB_KE_GALLERY").is_some() {
            debug!("DTB_KE_GALLERY set — opening the component gallery");
            components::gallery::open_gallery_window(cx);
        } else {
            app::open_main_window(cx);
        }

        // Debug aids: open a secondary window straight away.
        if let Some(v) = std::env::var_os("DTB_KE_ABOUT") {
            about::open_named(cx, &v.to_string_lossy());
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

        // Crash-handler smoke test: `DTB_KE_CRASH_TEST=segv|panic|bus[,thread]`
        // faults ~2 s after launch (on a worker thread with the `,thread`
        // suffix) so a `.dmp` should land under logs/crashes/.
        #[allow(clippy::manual_dangling_ptr)]
        if let Ok(kind) = std::env::var("DTB_KE_CRASH_TEST") {
            let on_thread = kind.contains(",thread");
            let fault = move || {
                std::thread::sleep(std::time::Duration::from_secs(2));
                eprintln!("DTB_KE_CRASH_TEST: faulting now ({kind})");
                match kind.split(',').next() {
                    Some("panic") => panic!("DTB_KE_CRASH_TEST: deliberate panic"),
                    // Deliberate faults — clippy's dangling/null warnings are the point.
                    Some("bus") => unsafe {
                        std::ptr::with_exposed_provenance_mut::<u64>(1).write_volatile(0)
                    },
                    _ => unsafe { std::ptr::null_mut::<u64>().write_volatile(0xdead) },
                }
            };
            if on_thread {
                std::thread::Builder::new()
                    .name("crash-test".into())
                    .spawn(fault)
                    .ok();
            } else {
                std::thread::spawn(fault);
            }
        }

        cx.activate(true);
    });
}
