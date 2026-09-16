//! The application menu bar, assembled from [`crate::actions`].
//!
//! [`build`] is platform-agnostic: it returns gpui [`Menu`]s with the right
//! labels, separators and enabled/disabled state for a given [`MenuState`].
//! *Rendering* is the platform seam's job ([`crate::skin::menu`]): macOS turns
//! these into a native `NSMenu`; Windows and Linux will draw their own bar from
//! the same model in later phases.

use gpui::{Menu, MenuItem, OsAction, SharedString};

use crate::actions::{app, edit, file, help, window};
use crate::i18n::{ActiveLocale, Locale};

/// Everything [`build`] needs to decide labels and enabled state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MenuState {
    /// A competition-editing window is focused. When `false` (e.g. the settings
    /// window is up) every document-related item is disabled.
    pub document_context: bool,
    /// A competition is currently selected.
    pub has_competition: bool,
    /// A judging table is selected in the detail view.
    pub has_table_selection: bool,
    /// The selected competition has an undo step available.
    pub can_undo: bool,
    /// The selected competition has a redo step available.
    pub can_redo: bool,
    /// The focused window is in fullscreen.
    pub fullscreen: bool,
    /// The preview window is open.
    pub preview_open: bool,
}

impl Default for MenuState {
    fn default() -> Self {
        Self {
            document_context: true,
            has_competition: false,
            has_table_selection: false,
            can_undo: false,
            can_redo: false,
            fullscreen: false,
            preview_open: false,
        }
    }
}

const APP_NAME: &str = "DTB Kampfrichtereinsatzpläne";

/// The label a command shows in the menu bar, looked up by its action name
/// (`"file::NewCompetition"`). Used by the settings window's key-binding list
/// and its conflict messages. Trailing " …" is trimmed.
pub fn label_for(action_name: &str, locale: &Locale) -> Option<SharedString> {
    fn scan(items: &[MenuItem], action_name: &str) -> Option<SharedString> {
        for item in items {
            match item {
                MenuItem::Action { name, action, .. } if action.name() == action_name => {
                    let label = name.as_ref().trim_end();
                    let label = label.strip_suffix('…').unwrap_or(label).trim_end();
                    return Some(SharedString::from(label.to_owned()));
                }
                MenuItem::Submenu(menu) => {
                    if let Some(found) = scan(&menu.items, action_name) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    build(MenuState::default(), locale)
        .iter()
        .find_map(|menu| scan(&menu.items, action_name))
}

/// Build the menu bar for `state`.
pub fn build(state: MenuState, locale: &Locale) -> Vec<Menu> {
    vec![
        app_menu(state, locale),
        file_menu(state, locale),
        edit_menu(state, locale),
        window_menu(state, locale),
        help_menu(state, locale),
    ]
}

/// A document-scoped item: enabled only in a document window and when `available`.
fn doc_item(
    label: impl Into<SharedString>,
    action: impl gpui::Action,
    state: MenuState,
    available: bool,
) -> MenuItem {
    MenuItem::action(label, action).disabled(!(state.document_context && available))
}

fn app_menu(state: MenuState, locale: &Locale) -> Menu {
    let mut items = vec![
        MenuItem::action(
            locale.t_fmt("actions.app::About", &[("app_name", APP_NAME)]),
            app::About,
        ),
        MenuItem::action(
            locale.t("actions.app::CheckForUpdates"),
            app::CheckForUpdates,
        )
        .disabled(!crate::updater::available()),
        MenuItem::separator(),
        MenuItem::action(locale.t("actions.app::OpenSettings"), app::OpenSettings),
    ];

    #[cfg(target_os = "macos")]
    {
        items.push(MenuItem::separator());
        items.push(MenuItem::os_submenu(
            locale.t("menu.app.services"),
            gpui::SystemMenuType::Services,
        ));
    }

    items.extend([
        MenuItem::separator(),
        MenuItem::action(
            locale.t_fmt("actions.app::HideApp", &[("app_name", APP_NAME)]),
            app::HideApp,
        ),
        MenuItem::action(locale.t("actions.app::HideOthers"), app::HideOthers),
        MenuItem::separator(),
        MenuItem::action(
            locale.t_fmt("actions.app::Quit", &[("app_name", APP_NAME)]),
            app::Quit,
        ),
    ]);

    let _ = state;
    Menu::new(APP_NAME).items(items)
}

fn file_menu(state: MenuState, locale: &Locale) -> Menu {
    let has_comp = state.has_competition;
    Menu::new(locale.t("menu.file.title")).items(vec![
        doc_item(
            locale.t("actions.file::NewCompetition"),
            file::NewCompetition,
            state,
            true,
        ),
        doc_item(
            locale.t("actions.file::AddJudgingTable"),
            file::AddJudgingTable,
            state,
            has_comp,
        ),
        MenuItem::separator(),
        doc_item(
            locale.t("actions.file::CompetitionSettings"),
            file::CompetitionSettings,
            state,
            has_comp,
        ),
        MenuItem::separator(),
        doc_item(
            locale.t("actions.file::ExportCompetition"),
            file::ExportCompetition,
            state,
            has_comp,
        ),
        doc_item(
            locale.t("actions.file::ImportCompetition"),
            file::ImportCompetition,
            state,
            true,
        ),
        doc_item(
            locale.t("actions.file::ExportAll"),
            file::ExportAll,
            state,
            true,
        ),
        doc_item(
            locale.t("actions.file::ShowTrash"),
            file::ShowTrash,
            state,
            true,
        ),
        MenuItem::separator(),
        MenuItem::action(locale.t("actions.file::CloseWindow"), file::CloseWindow),
    ])
}

fn edit_menu(state: MenuState, locale: &Locale) -> Menu {
    let has_sel = state.has_table_selection;
    Menu::new(locale.t("menu.edit.title")).items(vec![
        doc_item(
            locale.t("actions.edit::Undo"),
            edit::Undo,
            state,
            state.can_undo,
        ),
        doc_item(
            locale.t("actions.edit::Redo"),
            edit::Redo,
            state,
            state.can_redo,
        ),
        MenuItem::separator(),
        MenuItem::os_action(locale.t("actions.edit::Cut"), edit::Cut, OsAction::Cut),
        MenuItem::os_action(locale.t("actions.edit::Copy"), edit::Copy, OsAction::Copy),
        MenuItem::os_action(
            locale.t("actions.edit::Paste"),
            edit::Paste,
            OsAction::Paste,
        ),
        MenuItem::separator(),
        doc_item(
            locale.t("menu.edit.add-judging-table"),
            file::AddJudgingTable,
            state,
            state.has_competition,
        ),
        doc_item(
            locale.t("actions.edit::DuplicateJudgingTable"),
            edit::DuplicateJudgingTable,
            state,
            has_sel,
        ),
        doc_item(
            locale.t("actions.edit::DeleteJudgingTable"),
            edit::DeleteJudgingTable,
            state,
            has_sel,
        ),
    ])
}

fn window_menu(state: MenuState, locale: &Locale) -> Menu {
    let preview_label = if state.preview_open {
        locale.t("menu.window.close-preview")
    } else {
        locale.t("menu.window.open-preview")
    };
    let fullscreen_label = if state.fullscreen {
        locale.t("menu.window.exit-fullscreen")
    } else {
        locale.t("menu.window.enter-fullscreen")
    };
    Menu::new(locale.t("menu.window.title")).items(vec![
        MenuItem::action(preview_label, window::TogglePreview),
        MenuItem::action(fullscreen_label, window::ToggleFullscreen),
        MenuItem::separator(),
        MenuItem::action(locale.t("actions.window::Minimize"), window::Minimize),
    ])
}

fn help_menu(_state: MenuState, locale: &Locale) -> Menu {
    Menu::new(locale.t("menu.help.title")).items(vec![
        MenuItem::action(locale.t("actions.help::ReportBug"), help::ReportBug),
        MenuItem::action(
            locale.t("actions.help::RequestFeature"),
            help::RequestFeature,
        ),
        MenuItem::separator(),
        MenuItem::action(locale.t("actions.help::ShowLogs"), help::ShowLogs),
        MenuItem::action(locale.t("actions.help::OpenLogFolder"), help::OpenLogFolder),
        MenuItem::action(locale.t("actions.help::ShowDatabase"), help::ShowDatabase),
        MenuItem::separator(),
        MenuItem::action(
            locale.t("actions.help::ClearLogFolder"),
            help::ClearLogFolder,
        ),
        MenuItem::separator(),
        MenuItem::action(
            locale.t_fmt("actions.help::OpenRepository", &[("app_name", APP_NAME)]),
            help::OpenRepository,
        ),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Find an action item's disabled flag by its label (searches submenus one
    /// level deep, which is all we build).
    fn disabled(menus: &[Menu], label: &str) -> bool {
        menus
            .iter()
            .flat_map(|menu| &menu.items)
            .find_map(|item| match item {
                MenuItem::Action { name, disabled, .. } if name.as_ref() == label => {
                    Some(*disabled)
                }
                _ => None,
            })
            .unwrap_or_else(|| panic!("no menu item labelled {label:?}"))
    }

    #[test]
    fn builds_the_five_top_level_menus() {
        let locale = crate::i18n::test_locale();
        let menus = build(MenuState::default(), &locale);
        let names: Vec<_> = menus.iter().map(|m| m.name.to_string()).collect();
        assert_eq!(
            names,
            [
                "DTB Kampfrichtereinsatzpläne",
                "Ablage",
                "Bearbeiten",
                "Fenster",
                "Hilfe"
            ]
        );
    }

    #[test]
    fn competition_items_track_selection_state() {
        let locale = crate::i18n::test_locale();
        let none = MenuState::default();
        assert!(disabled(&build(none, &locale), "Wettkampfeinstellungen …"));
        assert!(disabled(&build(none, &locale), "Kampfgericht duplizieren"));

        let with_comp = MenuState {
            has_competition: true,
            ..MenuState::default()
        };
        assert!(!disabled(
            &build(with_comp, &locale),
            "Wettkampfeinstellungen …"
        ));
        assert!(disabled(
            &build(with_comp, &locale),
            "Kampfgericht duplizieren"
        ));

        let with_table = MenuState {
            has_competition: true,
            has_table_selection: true,
            ..MenuState::default()
        };
        assert!(!disabled(
            &build(with_table, &locale),
            "Kampfgericht duplizieren"
        ));
        assert!(!disabled(
            &build(with_table, &locale),
            "Kampfgericht löschen"
        ));
    }

    #[test]
    fn undo_redo_track_history_availability() {
        let locale = crate::i18n::test_locale();
        assert!(disabled(
            &build(MenuState::default(), &locale),
            "Rückgängig"
        ));
        assert!(disabled(
            &build(MenuState::default(), &locale),
            "Wiederholen"
        ));

        let can_undo = MenuState {
            has_competition: true,
            can_undo: true,
            ..MenuState::default()
        };
        assert!(!disabled(&build(can_undo, &locale), "Rückgängig"));
        assert!(disabled(&build(can_undo, &locale), "Wiederholen"));

        // A non-document window disables them even with history.
        let settings = MenuState {
            document_context: false,
            can_undo: true,
            can_redo: true,
            ..MenuState::default()
        };
        assert!(disabled(&build(settings, &locale), "Rückgängig"));
    }

    #[test]
    fn a_non_document_window_disables_document_items() {
        let locale = crate::i18n::test_locale();
        let settings_window = MenuState {
            document_context: false,
            has_competition: true,
            has_table_selection: true,
            ..MenuState::default()
        };
        let menus = build(settings_window, &locale);
        assert!(disabled(&menus, "Neuer Wettkampf"));
        assert!(disabled(&menus, "Kampfgericht duplizieren"));
        // App / window / help items stay usable.
        assert!(!disabled(&menus, "Minimieren"));
    }

    fn has_labelled_action(menus: &[Menu], label: &str) -> bool {
        menus
            .iter()
            .flat_map(|m| &m.items)
            .any(|item| matches!(item, MenuItem::Action { name, .. } if name.as_ref() == label))
    }

    #[test]
    fn fullscreen_label_flips() {
        let locale = crate::i18n::test_locale();
        assert!(has_labelled_action(
            &build(MenuState::default(), &locale),
            "Vollbild"
        ));
        assert!(has_labelled_action(
            &build(
                MenuState {
                    fullscreen: true,
                    ..MenuState::default()
                },
                &locale
            ),
            "Vollbild verlassen"
        ));
    }
}
