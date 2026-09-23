//! Default key bindings for the menu [`actions`](crate::actions), plus the
//! user's re-bindings.
//!
//! [`DEFAULTS`] is the authoritative table: one row per action, its default
//! keystroke (per platform), and whether the user may change it. User overrides
//! live in `Settings.toml`'s `[keybindings]` table, keyed by action name
//! (`"file::NewCompetition"`), and are layered on top for the *configurable*
//! rows only.
//!
//! ## Applying changes
//!
//! gpui's [`Keymap`](gpui_kit::Keymap) has no "remove one binding" operation, and
//! [`App::clear_key_bindings`](gpui_kit::App::clear_key_bindings) would also drop the
//! bindings `gpui-base` installs for its text editor. So [`rebind`] *appends*: a
//! targeted [`Unbind`](gpui_kit::Unbind) for the previously effective keystroke,
//! then the new binding. [`install`] (startup) builds the whole set once.

use std::collections::BTreeMap;
use std::rc::Rc;

use gpui_kit::{
    Action, App, DummyKeyboardMapper, KeyBinding, KeyBindingContextPredicate, Keystroke, Unbind,
};

use crate::actions::{app, edit, file, window};

/// A default keystroke that may differ between platforms.
#[derive(Clone, Copy)]
pub enum DefaultKey {
    /// The same on every platform. May use `secondary-` (⌘ on macOS, Ctrl
    /// elsewhere).
    All(&'static str),
    /// Distinct keystrokes for macOS vs. Windows/Linux.
    Split {
        mac: &'static str,
        other: &'static str,
    },
    /// No default binding.
    None,
}

impl DefaultKey {
    /// The keystroke for the platform this build targets.
    pub fn resolve(self) -> Option<&'static str> {
        match self {
            DefaultKey::All(k) => Some(k),
            DefaultKey::Split { mac, other } => Some(if cfg!(target_os = "macos") {
                mac
            } else {
                other
            }),
            DefaultKey::None => None,
        }
    }
}

/// One row of the keymap table.
pub struct Binding {
    /// A fresh instance of the action, used for its name and to build the
    /// binding.
    pub action: fn() -> Box<dyn Action>,
    pub default: DefaultKey,
    /// Whether the user is allowed to override this binding.
    pub configurable: bool,
    /// A gpui key-context predicate that must hold for the keyboard shortcut to
    /// fire (the menu item always dispatches the action regardless). Used to
    /// keep shortcuts that collide with the text editor — `backspace`, `cmd-z`,
    /// `cmd-x`… — from firing while a field is focused (`"!Input"`).
    pub context: Option<&'static str>,
}

impl Binding {
    pub fn name(&self) -> &'static str {
        (self.action)().name()
    }
}

macro_rules! binding {
    ($action:expr, $default:expr, $configurable:expr) => {
        binding!($action, $default, $configurable, None)
    };
    ($action:expr, $default:expr, $configurable:expr, $context:expr) => {
        Binding {
            action: || Box::new($action),
            default: $default,
            configurable: $configurable,
            context: $context,
        }
    };
}

/// The authoritative binding table. Order is irrelevant.
pub fn defaults() -> Vec<Binding> {
    use DefaultKey::{All, Split};
    vec![
        // -- app ---------------------------------------------------------------
        binding!(app::OpenSettings, All("secondary-,"), false),
        binding!(
            app::HideApp,
            Split {
                mac: "cmd-h",
                other: "ctrl-h"
            },
            false
        ),
        binding!(
            app::HideOthers,
            Split {
                mac: "cmd-alt-h",
                other: "ctrl-alt-h"
            },
            false
        ),
        binding!(
            app::Quit,
            Split {
                mac: "cmd-q",
                other: "alt-f4"
            },
            false
        ),
        // -- file --------------------------------------------------------------
        binding!(file::NewCompetition, All("secondary-n"), true),
        binding!(file::AddJudgingTable, All("secondary-shift-n"), true),
        binding!(file::CompetitionSettings, All("secondary-shift-,"), true),
        binding!(
            file::DeleteCompetitions,
            Split {
                mac: "cmd-backspace",
                other: "ctrl-delete"
            },
            true,
            Some("!Input")
        ),
        binding!(
            file::ExportCompetition,
            Split {
                mac: "cmd-e",
                other: "ctrl-e"
            },
            false
        ),
        binding!(
            file::ImportCompetition,
            Split {
                mac: "cmd-i",
                other: "ctrl-i"
            },
            false
        ),
        binding!(
            file::ExportAll,
            Split {
                mac: "cmd-alt-s",
                other: "ctrl-alt-s"
            },
            false
        ),
        binding!(
            file::CloseWindow,
            Split {
                mac: "cmd-shift-w",
                other: "ctrl-shift-w"
            },
            false
        ),
        // -- edit ------------------------------------------------------------
        // These collide with the text editor's own shortcuts; `"!Input"` keeps
        // them from firing while a field is focused (the menu items still work).
        binding!(edit::Undo, All("secondary-z"), true, Some("!Input")),
        binding!(
            edit::Redo,
            Split {
                mac: "cmd-shift-z",
                other: "ctrl-y"
            },
            true,
            Some("!Input")
        ),
        binding!(
            edit::Cut,
            Split {
                mac: "cmd-x",
                other: "ctrl-x"
            },
            false,
            Some("!Input")
        ),
        binding!(
            edit::Copy,
            Split {
                mac: "cmd-c",
                other: "ctrl-c"
            },
            false,
            Some("!Input")
        ),
        binding!(
            edit::Paste,
            Split {
                mac: "cmd-v",
                other: "ctrl-v"
            },
            false,
            Some("!Input")
        ),
        binding!(
            edit::DuplicateJudgingTable,
            All("secondary-shift-d"),
            true,
            Some("!Input")
        ),
        binding!(
            edit::DeleteJudgingTable,
            Split {
                mac: "backspace",
                other: "delete"
            },
            true,
            Some("!Input")
        ),
        // -- window ------------------------------------------------------------
        binding!(window::TogglePreview, All("secondary-shift-v"), true),
        binding!(window::ToggleSidebar, All("secondary-alt-s"), true),
        // Was `All("fn-f")` — macOS's own fullscreen chord, but "fn" isn't a
        // real modifier on Windows/Linux, leaving no keyboard way at all to
        // exit fullscreen there once the OS-drawn min/max/close buttons are
        // gone (real OS fullscreen removes window chrome on every platform,
        // not just ours — see the app.rs `ToggleFullscreen` handler). F11 is
        // the conventional fullscreen toggle on both.
        binding!(
            window::ToggleFullscreen,
            Split {
                mac: "fn-f",
                other: "f11"
            },
            false
        ),
        binding!(
            window::Minimize,
            Split {
                mac: "cmd-m",
                other: "ctrl-m"
            },
            false
        ),
        // -- help: no default bindings ---------------------------------------
    ]
}

/// User overrides: `action name → keystroke`. Deserialised straight from
/// `Settings.toml`.
pub type Overrides = BTreeMap<String, String>;

/// The configurable rows, in table order, paired with their translated menu
/// label. Powers the settings window's key-binding list.
pub fn configurable(locale: &crate::i18n::Locale) -> Vec<(&'static str, gpui_kit::SharedString)> {
    defaults()
        .into_iter()
        .filter(|row| row.configurable)
        .map(|row| {
            let name = row.name();
            (
                name,
                crate::menu::label_for(name, locale)
                    .unwrap_or_else(|| gpui_kit::SharedString::from(name)),
            )
        })
        .collect()
}

/// Parse a whole space-separated keystroke sequence, discarding `key_char`
/// (which varies by keyboard layout and must not affect equality).
fn parse_sequence(keystroke: &str) -> Option<Vec<(gpui_kit::Modifiers, String)>> {
    let chords: Vec<_> = keystroke.split_whitespace().collect();
    if chords.is_empty() {
        return None;
    }
    chords
        .into_iter()
        .map(|chord| Keystroke::parse(chord).ok().map(|k| (k.modifiers, k.key)))
        .collect()
}

/// Whether two keystroke strings denote the same chord sequence (modifiers +
/// key), regardless of spelling (`secondary-` vs `cmd-`, case, …).
pub fn same_keystroke(a: &str, b: &str) -> bool {
    match (parse_sequence(a), parse_sequence(b)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// A reason a candidate keystroke should not be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Conflict {
    /// Already bound to another of our commands.
    Command(String),
    /// Shadows a well-known OS / global shortcut.
    Reserved(&'static str),
}

impl Conflict {
    /// A localized sentence for the settings window.
    pub fn message(&self, locale: &crate::i18n::Locale) -> String {
        use crate::i18n::ActiveLocale;
        match self {
            Conflict::Command(label) => {
                locale.t_fmt("keymap.conflict-command", &[("label", label.as_str())])
            }
            Conflict::Reserved(key) => {
                let what = locale.t(key);
                locale.t_fmt("keymap.conflict-reserved", &[("what", what.as_ref())])
            }
        }
    }
}

/// Well-known keystrokes that a user binding should not shadow. Not exhaustive
/// — the common system and clipboard shortcuts people actually hit. The
/// second element of each pair is a translation key (`reserved.*`), not
/// literal text — resolved by [`Conflict::message`].
fn reserved_keystrokes() -> &'static [(&'static str, &'static str)] {
    #[cfg(target_os = "macos")]
    {
        &[
            ("cmd-q", "reserved.quit"),
            ("cmd-w", "reserved.close-window"),
            ("cmd-h", "reserved.hide-app"),
            ("cmd-m", "reserved.minimize-window"),
            ("cmd-,", "reserved.preferences"),
            ("cmd-a", "reserved.select-all"),
            ("cmd-z", "reserved.undo"),
            ("cmd-x", "reserved.cut"),
            ("cmd-c", "reserved.copy"),
            ("cmd-v", "reserved.paste"),
            ("cmd-tab", "reserved.switch-app"),
            ("cmd-space", "reserved.spotlight"),
            ("cmd-ctrl-f", "reserved.fullscreen"),
            ("cmd-shift-3", "reserved.screenshot"),
            ("cmd-shift-4", "reserved.screenshot"),
            ("cmd-shift-5", "reserved.screen-recording"),
        ]
    }
    #[cfg(not(target_os = "macos"))]
    {
        &[
            ("ctrl-w", "reserved.close-window"),
            ("ctrl-a", "reserved.select-all"),
            ("ctrl-z", "reserved.undo"),
            ("ctrl-x", "reserved.cut"),
            ("ctrl-c", "reserved.copy"),
            ("ctrl-v", "reserved.paste"),
            ("alt-f4", "reserved.close-window-system"),
            ("ctrl-alt-delete", "reserved.system"),
            ("super-l", "reserved.lock"),
        ]
    }
}

/// Check `candidate` for `action_name`, given `overrides` as the *pending*
/// override map (the edit the user is considering). Returns the first conflict.
pub fn conflict(
    action_name: &str,
    candidate: &str,
    overrides: &Overrides,
    locale: &crate::i18n::Locale,
) -> Option<Conflict> {
    if !is_valid_keystroke(candidate) {
        return None;
    }
    for (reserved, what) in reserved_keystrokes() {
        if same_keystroke(candidate, reserved) {
            return Some(Conflict::Reserved(what));
        }
    }
    for row in defaults() {
        let name = row.name();
        if name == action_name {
            continue;
        }
        let Some(effective) = effective_keystroke(name, overrides) else {
            continue;
        };
        if same_keystroke(candidate, &effective) {
            let label = crate::menu::label_for(name, locale)
                .unwrap_or_else(|| gpui_kit::SharedString::from(name))
                .to_string();
            return Some(Conflict::Command(label));
        }
    }
    None
}

/// Whether `keystroke` parses as a gpui keystroke sequence (space-separated).
pub fn is_valid_keystroke(keystroke: &str) -> bool {
    let trimmed = keystroke.trim();
    !trimmed.is_empty()
        && trimmed
            .split_whitespace()
            .all(|chord| Keystroke::parse(chord).is_ok())
}

/// The keystroke that is *actually* in effect for `action_name`, taking a valid
/// override into account. `None` if the action has no binding.
pub fn effective_keystroke(action_name: &str, overrides: &Overrides) -> Option<String> {
    let row = defaults().into_iter().find(|b| b.name() == action_name)?;
    if row.configurable
        && let Some(custom) = overrides.get(action_name)
        && is_valid_keystroke(custom)
    {
        return Some(custom.clone());
    }
    row.default.resolve().map(str::to_owned)
}

/// The default keystroke for `action_name` on this platform (ignoring any
/// override).
pub fn default_keystroke(action_name: &str) -> Option<String> {
    defaults()
        .into_iter()
        .find(|b| b.name() == action_name)
        .and_then(|row| row.default.resolve().map(str::to_owned))
}

fn parse_context(context: Option<&str>) -> Option<Rc<KeyBindingContextPredicate>> {
    let raw = context?;
    match KeyBindingContextPredicate::parse(raw) {
        Ok(predicate) => Some(Rc::new(predicate)),
        Err(err) => {
            eprintln!("keymap: ignoring invalid key context {raw:?}: {err}");
            None
        }
    }
}

fn make_binding(
    keystroke: &str,
    action: Box<dyn Action>,
    context: Option<&str>,
) -> Option<KeyBinding> {
    let predicate = parse_context(context);
    match KeyBinding::load(
        keystroke,
        action,
        predicate,
        false,
        None,
        &DummyKeyboardMapper,
    ) {
        Ok(binding) => Some(binding),
        Err(err) => {
            eprintln!("keymap: ignoring invalid keystroke {keystroke:?}: {err}");
            None
        }
    }
}

/// Build the full set of menu-action bindings for the current platform, applying
/// `overrides` to the configurable rows. Invalid overrides are ignored (the
/// default is used instead).
pub fn key_bindings(overrides: &Overrides) -> Vec<KeyBinding> {
    let mut out = Vec::new();
    for row in defaults() {
        let name = row.name();
        let keystroke = if row.configurable {
            overrides
                .get(name)
                .filter(|k| is_valid_keystroke(k))
                .cloned()
                .or_else(|| row.default.resolve().map(str::to_owned))
        } else {
            row.default.resolve().map(str::to_owned)
        };
        let Some(keystroke) = keystroke else { continue };
        if let Some(binding) = make_binding(&keystroke, (row.action)(), row.context) {
            out.push(binding);
        }
    }
    out
}

/// Install the whole keymap at startup.
pub fn install(overrides: &Overrides, cx: &mut App) {
    cx.bind_keys(key_bindings(overrides));
}

/// Change one configurable binding at runtime: suppress its previously effective
/// keystroke and append the new one. `new_keystroke == None` clears the binding
/// (falls back to nothing until the next full [`install`]).
///
/// Returns `false` (and does nothing) for an unknown or non-configurable action,
/// or an unparseable keystroke.
pub fn rebind(
    action_name: &str,
    new_keystroke: Option<&str>,
    previous: &Overrides,
    cx: &mut App,
) -> bool {
    let Some(row) = defaults().into_iter().find(|b| b.name() == action_name) else {
        return false;
    };
    if !row.configurable {
        return false;
    }
    if let Some(k) = new_keystroke
        && !is_valid_keystroke(k)
    {
        return false;
    }

    let mut batch = Vec::new();
    if let Some(old) = effective_keystroke(action_name, previous) {
        // A targeted Unbind removes the earlier binding for this keystroke that
        // dispatches this action, regardless of context.
        if let Some(unbind) =
            make_binding(&old, Box::new(Unbind(action_name.to_string().into())), None)
        {
            batch.push(unbind);
        }
    }
    if let Some(new_keystroke) = new_keystroke
        && let Some(binding) = make_binding(new_keystroke, (row.action)(), row.context)
    {
        batch.push(binding);
    }

    if !batch.is_empty() {
        cx.bind_keys(batch);
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn same_keystroke_normalises_spellings() {
        assert!(same_keystroke(
            "secondary-n",
            if cfg!(target_os = "macos") {
                "cmd-n"
            } else {
                "ctrl-n"
            }
        ));
        assert!(same_keystroke("cmd-shift-a", "cmd-shift-a"));
        assert!(same_keystroke("ctrl-x ctrl-s", "ctrl-x ctrl-s"));
        assert!(!same_keystroke("cmd-a", "cmd-b"));
        assert!(!same_keystroke("cmd-a", "cmd-shift-a"));
        assert!(!same_keystroke("cmd-x cmd-s", "cmd-x"));
    }

    #[test]
    fn configurable_rows_have_labels() {
        let locale = crate::i18n::test_locale();
        let rows = configurable(&locale);
        assert!(!rows.is_empty());
        // Every configurable row resolves to a real translated menu label.
        for (name, label) in rows {
            assert_ne!(label.as_ref(), name, "no menu label found for {name}");
        }
    }

    #[test]
    fn conflict_detects_a_command_collision() {
        let locale = crate::i18n::test_locale();
        // `file::NewCompetition` defaults to secondary-n; try to bind
        // `file::AddJudgingTable` to the same chord.
        let target = file::AddJudgingTable.name();
        match conflict(target, "secondary-n", &Overrides::new(), &locale) {
            Some(Conflict::Command(label)) => assert!(!label.is_empty()),
            other => panic!("expected a command conflict, got {other:?}"),
        }
        // Its own default is fine.
        assert!(conflict(target, "secondary-shift-n", &Overrides::new(), &locale).is_none());
    }

    #[test]
    fn conflict_flags_a_reserved_shortcut() {
        let locale = crate::i18n::test_locale();
        let target = file::NewCompetition.name();
        let reserved = if cfg!(target_os = "macos") {
            "cmd-q"
        } else {
            "ctrl-w"
        };
        assert!(matches!(
            conflict(target, reserved, &Overrides::new(), &locale),
            Some(Conflict::Reserved(_))
        ));
    }

    #[test]
    fn every_default_keystroke_parses_and_names_are_unique() {
        let mut seen = std::collections::BTreeSet::new();
        for row in defaults() {
            let name = row.name();
            assert!(seen.insert(name), "duplicate action in table: {name}");
            if let Some(keystroke) = row.default.resolve() {
                assert!(
                    is_valid_keystroke(keystroke),
                    "invalid default keystroke {keystroke:?} for {name}"
                );
            }
        }
    }

    #[test]
    fn is_valid_keystroke_shapes() {
        assert!(is_valid_keystroke("cmd-n"));
        assert!(is_valid_keystroke("secondary-shift-,"));
        assert!(is_valid_keystroke("ctrl-x ctrl-s")); // a chord sequence
        assert!(!is_valid_keystroke(""));
        assert!(!is_valid_keystroke("   "));
        assert!(!is_valid_keystroke("a-b"));
    }

    #[test]
    fn overrides_apply_only_to_configurable_actions() {
        let new_competition = file::NewCompetition.name();
        let export = file::ExportCompetition.name();

        let mut overrides = Overrides::new();
        overrides.insert(new_competition.to_owned(), "secondary-shift-k".to_owned());
        overrides.insert(export.to_owned(), "secondary-shift-x".to_owned());

        assert_eq!(
            effective_keystroke(new_competition, &overrides).as_deref(),
            Some("secondary-shift-k"),
            "configurable action should honour the override"
        );
        assert_eq!(
            effective_keystroke(export, &overrides).as_deref(),
            DefaultKey::Split {
                mac: "cmd-e",
                other: "ctrl-e"
            }
            .resolve(),
            "fixed action should ignore the override"
        );
    }

    #[test]
    fn invalid_override_falls_back_to_default() {
        let name = file::NewCompetition.name();
        let mut overrides = Overrides::new();
        overrides.insert(name.to_owned(), "not-a-key!".to_owned());
        assert_eq!(
            effective_keystroke(name, &overrides).as_deref(),
            DefaultKey::All("secondary-n").resolve()
        );
    }

    #[test]
    fn key_bindings_builds_without_panicking() {
        // `KeyBinding::load` panics on some malformed input shapes; make sure the
        // whole default table survives a build.
        let bindings = key_bindings(&Overrides::new());
        assert!(bindings.len() >= 15);
    }

    #[test]
    fn editor_colliding_shortcuts_are_context_gated() {
        // backspace / cmd-z / cmd-x… must not fire while a text field is focused.
        for name in [
            edit::DeleteJudgingTable.name(),
            edit::Undo.name(),
            edit::Cut.name(),
            edit::Copy.name(),
            edit::Paste.name(),
        ] {
            let row = defaults().into_iter().find(|b| b.name() == name).unwrap();
            assert_eq!(
                row.context,
                Some("!Input"),
                "{name} shortcut would clash with the text editor"
            );
        }
        // …and the predicate itself is valid.
        assert!(parse_context(Some("!Input")).is_some());
    }
}
