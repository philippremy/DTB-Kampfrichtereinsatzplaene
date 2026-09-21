//! Every application command, expressed as a gpui [`Action`](gpui::Action).
//!
//! The actions are grouped into modules by the menu they live in. Their default
//! key bindings — and which of them the user may re-bind — are owned by
//! [`crate::keymap`]; [`crate::menu`] assembles them into the platform menu bar.
//!
//! Action *names* (`"<namespace>::<Struct>"`, e.g. `"file::NewCompetition"`) are
//! the stable identifiers used as keys in `Settings.toml`'s `[keybindings]`
//! table, so renaming a struct or its namespace is a breaking change to a user's
//! saved overrides.

use gpui::{App, actions};

use crate::filesystem::FilesystemHelper;

/// The project's public repository, opened by [`help::OpenRepository`].
pub const REPOSITORY_URL: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene";

/// Application-level commands (the app / "DTB Kampfrichtereinsatzpläne" menu).
pub mod app {
    use super::actions;

    actions!(
        app,
        [
            /// Open the "Über DTB Kampfrichtereinsatzpläne" window.
            About,
            /// Check for application updates now (handled on `AppShell`).
            CheckForUpdates,
            /// Open the settings window.
            OpenSettings,
            /// Hide all of the application's windows.
            HideApp,
            /// Hide every *other* application's windows (macOS).
            HideOthers,
            /// Quit the application.
            Quit,
        ]
    );
}

/// The File menu.
pub mod file {
    use super::actions;

    actions!(
        file,
        [
            /// Create a fresh, blank competition and select it.
            NewCompetition,
            /// Add a judging table to the active phase (also in the Edit menu).
            AddJudgingTable,
            /// Open the competition-metadata dialog for the selected competition.
            CompetitionSettings,
            /// Export the selected competition — a native save dialog picks the
            /// format (PDF / DOCX / `.dtbke` blob) and its options.
            ExportCompetition,
            /// Import a previously exported `.dtbke` file (warns before overwriting).
            ImportCompetition,
            /// Back up the whole database (a plain file copy).
            ExportAll,
            /// Open the trash window (soft-deleted competitions).
            ShowTrash,
            /// Move the selected competitions to the trash (after a confirmation).
            DeleteCompetitions,
            /// Placeholder for the informational database-status menu line. It
            /// has no handler on purpose, so the item is always greyed out.
            DatabaseStatus,
            /// Close the focused window.
            CloseWindow,
        ]
    );
}

/// The Edit menu.
pub mod edit {
    use super::actions;

    actions!(
        edit,
        [
            /// Undo the last change to the selected competition.
            Undo,
            /// Redo the last undone change to the selected competition.
            Redo,
            /// Cut the selection (text field or judging table).
            Cut,
            /// Copy the selection (text field or judging table).
            Copy,
            /// Paste (text field or judging table).
            Paste,
            /// Duplicate the selected judging table.
            DuplicateJudgingTable,
            /// Delete the selected judging table.
            DeleteJudgingTable,
        ]
    );
}

/// The Window menu.
pub mod window {
    use super::actions;

    actions!(
        window,
        [
            /// Open or close the preview window.
            TogglePreview,
            /// Enter or leave fullscreen.
            ToggleFullscreen,
            /// Minimise the focused window.
            Minimize,
        ]
    );
}

/// Register the handlers for app- and help-menu commands that don't need a
/// window (quit, hide, open a folder, …). Document- and window-scoped commands
/// are handled by `AppShell`.
///
/// `app::CheckForUpdates` is handled on `AppShell` (it needs the `Updater`
/// entity), so it is deliberately absent here.
pub fn register_global_handlers(cx: &mut App) {
    cx.on_action(|_: &app::Quit, cx: &mut App| {
        log::info!("quit requested");
        cx.quit()
    });
    cx.on_action(|_: &app::HideApp, cx: &mut App| cx.hide());
    cx.on_action(|_: &app::HideOthers, cx: &mut App| cx.hide_other_apps());
    cx.on_action(|_: &app::About, cx: &mut App| crate::about::open(cx));
    cx.on_action(|_: &app::OpenSettings, cx: &mut App| crate::settings_window::open(cx));
    cx.on_action(|_: &help::ShowLogs, cx: &mut App| crate::logs_window::open(cx));
    cx.on_action(|_: &help::ReportBug, cx: &mut App| crate::feedback_window::open_bug(cx));
    cx.on_action(|_: &help::RequestFeature, cx: &mut App| crate::feedback_window::open_feature(cx));

    cx.on_action(|_: &help::OpenLogFolder, cx: &mut App| {
        log::debug!("revealing the log folder");
        cx.open_with_system(FilesystemHelper::instance().get_log_dir());
    });
    cx.on_action(|_: &help::ShowDatabase, cx: &mut App| {
        log::debug!("revealing the database file");
        cx.reveal_path(&FilesystemHelper::instance().database_path());
    });
    cx.on_action(|_: &help::ClearLogFolder, _: &mut App| clear_log_folder());
    cx.on_action(|_: &help::OpenRepository, cx: &mut App| {
        log::debug!("opening the repository URL");
        cx.open_url(REPOSITORY_URL)
    });
}

/// Delete every file in the log folder except the one the current session is
/// still writing to.
fn clear_log_folder() {
    let helper = FilesystemHelper::instance();
    let active = helper.get_log_file();
    let dir = helper.get_log_dir();
    log::info!("clearing the log folder (keeping the active session file)");
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(err) => {
            log::warn!("could not read the log folder {}: {err}", dir.display());
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_file()
            && path != active
            && let Err(err) = std::fs::remove_file(&path)
        {
            log::warn!(
                "Logdatei {} konnte nicht gelöscht werden: {err}",
                path.display()
            );
        }
    }
}

/// The Help menu.
pub mod help {
    use super::actions;

    actions!(
        help,
        [
            /// Open the bug-report window.
            ReportBug,
            /// Open the feature-request window.
            RequestFeature,
            /// Open the log-viewer window.
            ShowLogs,
            /// Reveal the log folder in the system file manager.
            OpenLogFolder,
            /// Reveal the database file in the system file manager.
            ShowDatabase,
            /// Delete every file in the log folder.
            ClearLogFolder,
            /// Open the project's Codeberg repository in the default browser.
            OpenRepository,
        ]
    );
}
