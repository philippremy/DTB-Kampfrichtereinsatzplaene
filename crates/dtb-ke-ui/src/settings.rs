//! Persisted user settings — a small `Settings.toml` in the application data
//! directory. Missing or unreadable falls back to defaults.
//!
//! The loaded value is also installed as a gpui [`Global`] ([`Settings::init`]),
//! so any view can read the current settings with [`Settings::global`] and the
//! settings window mutates them through [`Settings::update`] (which persists and
//! replaces the global in one step).

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

use gpui::{App, Global};
use serde::{Deserialize, Serialize};

use crate::filesystem::FilesystemHelper;
use crate::theme::ThemeMode;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    /// The user's light / dark preference.
    pub theme_mode: ThemeMode,
    /// Snap every animation to its end state (accessibility / low-power).
    pub reduce_motion: bool,
    /// How long editing must be quiet before the autosave writes the blob.
    pub autosave: AutosaveDelay,
    /// Overrides the logger's automatic level detection (see [`dtb_ke_log`]).
    pub log_level: LogLevel,
    /// Menu-command key-binding overrides: `"file::NewCompetition" → "cmd-shift-n"`.
    /// Only the *configurable* commands are honoured (see [`crate::keymap`]).
    pub keybindings: BTreeMap<String, String>,
    /// Check for application updates in the background (see [`crate::updater`]).
    /// Default **on**.
    pub auto_update: bool,
    /// A release version the user chose to skip — the update toast stays hidden
    /// until a *newer* version than this appears. Cleared on a channel switch
    /// (see [`UpdateChannel`]) — a skip recorded on one channel doesn't apply to
    /// the other's, unrelated, version sequence.
    pub skipped_update: Option<String>,
    /// Which release channel the updater polls. Default `Stable`.
    pub update_channel: UpdateChannel,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme_mode: ThemeMode::default(),
            reduce_motion: false,
            autosave: AutosaveDelay::default(),
            log_level: LogLevel::default(),
            keybindings: BTreeMap::new(),
            auto_update: true,
            skipped_update: None,
            update_channel: UpdateChannel::default(),
        }
    }
}

/// Which manifest the updater polls — see `crate::updater`'s two build tracks:
/// tagged releases (LTO, full bundle set, notarized on macOS) vs. every commit
/// to `main` (LTO off, ad-hoc signed only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Tip,
}

impl UpdateChannel {
    pub const ALL: [Self; 2] = [Self::Stable, Self::Tip];

    /// A German label for the picker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Stable => "Stabil",
            Self::Tip => "Tip (Nightly)",
        }
    }

    /// A short explanation for the settings window.
    pub fn description(self) -> &'static str {
        match self {
            Self::Stable => {
                "Veröffentlichte Versionen — geprüft, signiert und (auf macOS) notariell beglaubigt."
            }
            Self::Tip => {
                "Jeder Commit auf dem Hauptzweig — ungetestet, nur ad-hoc signiert; auf macOS daher \
                           ohne automatische Installation (nur Download)."
            }
        }
    }
}

impl Global for Settings {}

impl Settings {
    fn path() -> PathBuf {
        FilesystemHelper::instance()
            .get_data_dir()
            .join("Settings.toml")
    }

    /// Load the settings file, or defaults if it is missing or invalid.
    pub fn load() -> Self {
        let path = Self::path();
        let src = match std::fs::read_to_string(&path) {
            Ok(src) => src,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                log::debug!("settings: no {} yet — using defaults", path.display());
                return Self::default();
            }
            Err(err) => {
                log::warn!(
                    "settings: {} unreadable ({err}) — using defaults",
                    path.display()
                );
                return Self::default();
            }
        };
        match toml::from_str(&src) {
            Ok(settings) => settings,
            Err(err) => {
                log::warn!(
                    "settings: {} is invalid ({err}) — using defaults",
                    path.display()
                );
                Self::default()
            }
        }
    }

    fn save(&self) {
        let path = Self::path();
        match toml::to_string_pretty(self) {
            Ok(src) => match std::fs::write(&path, src) {
                Ok(()) => log::debug!("settings written to {}", path.display()),
                Err(err) => log::error!("settings: could not write {}: {err}", path.display()),
            },
            Err(err) => log::error!("settings: could not serialize: {err}"),
        }
    }

    /// Install the persisted settings as the gpui global. Call once at startup.
    pub fn init(cx: &mut App) {
        cx.set_global(Self::load());
    }

    /// The current settings global. Falls back to defaults if it has not been
    /// installed yet (only reachable from tests).
    pub fn global(cx: &App) -> Settings {
        cx.try_global::<Settings>().cloned().unwrap_or_default()
    }

    /// Mutate the settings, persist the file, and replace the global.
    pub fn update(cx: &mut App, edit: impl FnOnce(&mut Settings)) {
        let mut settings = cx
            .try_global::<Settings>()
            .cloned()
            .unwrap_or_else(Self::load);
        edit(&mut settings);
        log::trace!("settings updated");
        settings.save();
        cx.set_global(settings);
    }

    /// The effective autosave debounce — reads the global, defaulting sensibly.
    pub fn autosave_delay(cx: &App) -> Duration {
        Self::global(cx).autosave.duration()
    }
}

/// The autosave debounce, as a small set of presets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AutosaveDelay {
    /// Write as soon as the edit lands.
    Immediate,
    /// 0.5 s — the historical default.
    #[default]
    Half,
    One,
    Two,
    Five,
}

impl AutosaveDelay {
    pub const ALL: [Self; 5] = [
        Self::Immediate,
        Self::Half,
        Self::One,
        Self::Two,
        Self::Five,
    ];

    pub fn duration(self) -> Duration {
        Duration::from_millis(match self {
            Self::Immediate => 0,
            Self::Half => 500,
            Self::One => 1_000,
            Self::Two => 2_000,
            Self::Five => 5_000,
        })
    }

    /// A German label for the picker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Immediate => "Sofort",
            Self::Half => "Nach 0,5 Sekunden",
            Self::One => "Nach 1 Sekunde",
            Self::Two => "Nach 2 Sekunden",
            Self::Five => "Nach 5 Sekunden",
        }
    }
}

/// The log-level setting. `Auto` follows the logger's own detection
/// (debug/release default + `DTB_KE_LOG_LEVEL`); the rest force a level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    #[default]
    Auto,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevel {
    pub const ALL: [Self; 6] = [
        Self::Auto,
        Self::Error,
        Self::Warn,
        Self::Info,
        Self::Debug,
        Self::Trace,
    ];

    /// `None` for [`LogLevel::Auto`]; otherwise the forced filter.
    pub fn to_filter(self) -> Option<log::LevelFilter> {
        match self {
            Self::Auto => None,
            Self::Error => Some(log::LevelFilter::Error),
            Self::Warn => Some(log::LevelFilter::Warn),
            Self::Info => Some(log::LevelFilter::Info),
            Self::Debug => Some(log::LevelFilter::Debug),
            Self::Trace => Some(log::LevelFilter::Trace),
        }
    }

    /// A short German label for the picker.
    pub fn label(self) -> &'static str {
        match self {
            Self::Auto => "Automatisch",
            Self::Error => "Fehler",
            Self::Warn => "Warnung",
            Self::Info => "Info",
            Self::Debug => "Debug",
            Self::Trace => "Trace",
        }
    }
}
