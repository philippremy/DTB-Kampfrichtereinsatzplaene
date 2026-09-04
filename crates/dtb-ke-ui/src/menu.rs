//! The application menu bar, assembled from [`crate::actions`].
//!
//! [`build`] is platform-agnostic: it returns gpui [`Menu`]s with the right
//! labels, separators and enabled/disabled state for a given [`MenuState`].
//! *Rendering* is the platform seam's job ([`crate::skin::menu`]): macOS turns
//! these into a native `NSMenu`; Windows and Linux will draw their own bar from
//! the same model in later phases.

use gpui::{Menu, MenuItem, OsAction};

use crate::actions::{app, edit, file, help, window};

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

/// The German label a command shows in the menu bar, looked up by its action
/// name (`"file::NewCompetition"`). Used by the settings window's key-binding
/// list and its conflict messages. Trailing " …" is trimmed.
pub fn label_for(action_name: &str) -> Option<String> {
    fn scan(items: &[MenuItem], action_name: &str) -> Option<String> {
        for item in items {
            match item {
                MenuItem::Action { name, action, .. } if action.name() == action_name => {
                    let label = name.as_ref().trim_end();
                    let label = label.strip_suffix('…').unwrap_or(label).trim_end();
                    return Some(label.to_string());
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
    build(MenuState::default())
        .iter()
        .find_map(|menu| scan(&menu.items, action_name))
}

/// Build the menu bar for `state`.
pub fn build(state: MenuState) -> Vec<Menu> {
    vec![
        app_menu(state),
        file_menu(state),
        edit_menu(state),
        window_menu(state),
        help_menu(state),
    ]
}

/// A document-scoped item: enabled only in a document window and when `available`.
fn doc_item(
    label: impl Into<String>,
    action: impl gpui::Action,
    state: MenuState,
    available: bool,
) -> MenuItem {
    MenuItem::action(label.into(), action).disabled(!(state.document_context && available))
}

fn app_menu(state: MenuState) -> Menu {
    let mut items = vec![
        MenuItem::action(format!("Über {APP_NAME}"), app::About),
        MenuItem::action("Nach Updates suchen …", app::CheckForUpdates)
            .disabled(!crate::updater::available()),
        MenuItem::separator(),
        MenuItem::action("Einstellungen …", app::OpenSettings),
    ];

    #[cfg(target_os = "macos")]
    {
        items.push(MenuItem::separator());
        items.push(MenuItem::os_submenu(
            "Dienste",
            gpui::SystemMenuType::Services,
        ));
    }

    items.extend([
        MenuItem::separator(),
        MenuItem::action(format!("{APP_NAME} ausblenden"), app::HideApp),
        MenuItem::action("Andere ausblenden", app::HideOthers),
        MenuItem::separator(),
        MenuItem::action(format!("{APP_NAME} beenden"), app::Quit),
    ]);

    let _ = state;
    Menu::new(APP_NAME).items(items)
}

fn file_menu(state: MenuState) -> Menu {
    let has_comp = state.has_competition;
    Menu::new("Ablage").items(vec![
        doc_item("Neuer Wettkampf", file::NewCompetition, state, true),
        doc_item("Neues Kampfgericht", file::AddJudgingTable, state, has_comp),
        MenuItem::separator(),
        doc_item(
            "Wettkampfeinstellungen …",
            file::CompetitionSettings,
            state,
            has_comp,
        ),
        MenuItem::separator(),
        doc_item(
            "Wettkampf exportieren …",
            file::ExportCompetition,
            state,
            has_comp,
        ),
        doc_item(
            "Wettkampf importieren …",
            file::ImportCompetition,
            state,
            true,
        ),
        doc_item("Gesamte Datenbank sichern …", file::ExportAll, state, true),
        MenuItem::separator(),
        MenuItem::action("Fenster schließen", file::CloseWindow),
    ])
}

fn edit_menu(state: MenuState) -> Menu {
    let has_sel = state.has_table_selection;
    Menu::new("Bearbeiten").items(vec![
        doc_item("Rückgängig", edit::Undo, state, state.can_undo),
        doc_item("Wiederholen", edit::Redo, state, state.can_redo),
        MenuItem::separator(),
        MenuItem::os_action("Ausschneiden", edit::Cut, OsAction::Cut),
        MenuItem::os_action("Kopieren", edit::Copy, OsAction::Copy),
        MenuItem::os_action("Einfügen", edit::Paste, OsAction::Paste),
        MenuItem::separator(),
        doc_item(
            "Kampfgericht hinzufügen",
            file::AddJudgingTable,
            state,
            state.has_competition,
        ),
        doc_item(
            "Kampfgericht duplizieren",
            edit::DuplicateJudgingTable,
            state,
            has_sel,
        ),
        doc_item(
            "Kampfgericht löschen",
            edit::DeleteJudgingTable,
            state,
            has_sel,
        ),
    ])
}

fn window_menu(state: MenuState) -> Menu {
    let preview_label = if state.preview_open {
        "Vorschaufenster schließen"
    } else {
        "Vorschaufenster öffnen"
    };
    let fullscreen_label = if state.fullscreen {
        "Vollbild verlassen"
    } else {
        "Vollbild"
    };
    Menu::new("Fenster").items(vec![
        MenuItem::action(preview_label, window::TogglePreview),
        MenuItem::action(fullscreen_label, window::ToggleFullscreen),
        MenuItem::separator(),
        MenuItem::action("Minimieren", window::Minimize),
    ])
}

fn help_menu(_state: MenuState) -> Menu {
    Menu::new("Hilfe").items(vec![
        MenuItem::action("Bug melden", help::ReportBug),
        MenuItem::action("Funktion anfragen", help::RequestFeature),
        MenuItem::separator(),
        MenuItem::action("Logs zeigen", help::ShowLogs),
        MenuItem::action("Logordner öffnen", help::OpenLogFolder),
        MenuItem::action("Datenbank zeigen", help::ShowDatabase),
        MenuItem::separator(),
        MenuItem::action("Logordner leeren", help::ClearLogFolder),
        MenuItem::separator(),
        MenuItem::action(format!("{APP_NAME} auf Codeberg"), help::OpenRepository),
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
        let menus = build(MenuState::default());
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
        let none = MenuState::default();
        assert!(disabled(&build(none), "Wettkampfeinstellungen …"));
        assert!(disabled(&build(none), "Kampfgericht duplizieren"));

        let with_comp = MenuState {
            has_competition: true,
            ..MenuState::default()
        };
        assert!(!disabled(&build(with_comp), "Wettkampfeinstellungen …"));
        assert!(disabled(&build(with_comp), "Kampfgericht duplizieren"));

        let with_table = MenuState {
            has_competition: true,
            has_table_selection: true,
            ..MenuState::default()
        };
        assert!(!disabled(&build(with_table), "Kampfgericht duplizieren"));
        assert!(!disabled(&build(with_table), "Kampfgericht löschen"));
    }

    #[test]
    fn undo_redo_track_history_availability() {
        assert!(disabled(&build(MenuState::default()), "Rückgängig"));
        assert!(disabled(&build(MenuState::default()), "Wiederholen"));

        let can_undo = MenuState {
            has_competition: true,
            can_undo: true,
            ..MenuState::default()
        };
        assert!(!disabled(&build(can_undo), "Rückgängig"));
        assert!(disabled(&build(can_undo), "Wiederholen"));

        // A non-document window disables them even with history.
        let settings = MenuState {
            document_context: false,
            can_undo: true,
            can_redo: true,
            ..MenuState::default()
        };
        assert!(disabled(&build(settings), "Rückgängig"));
    }

    #[test]
    fn a_non_document_window_disables_document_items() {
        let settings_window = MenuState {
            document_context: false,
            has_competition: true,
            has_table_selection: true,
            ..MenuState::default()
        };
        let menus = build(settings_window);
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
        assert!(has_labelled_action(
            &build(MenuState::default()),
            "Vollbild"
        ));
        assert!(has_labelled_action(
            &build(MenuState {
                fullscreen: true,
                ..MenuState::default()
            }),
            "Vollbild verlassen"
        ));
    }
}
