//! The DTB Kampfrichtereinsatzpläne UI as a library: the app (`main.rs`) and `dtb-ke-debugger` share
//! the theme / skin / component layer, settings, i18n and window plumbing from here.

// The UI layer is still being built out — the backend and theme layer expose
// more API than the current (stub) views consume.
#![allow(dead_code)]

pub mod about;
pub mod actions;
pub mod app;
pub mod build_info;
pub mod components;
pub mod crash_countdown;
pub mod crash_hints;
pub mod crash_report;
pub mod debug;
pub mod detail;
pub mod fault;
pub mod feedback_window;
pub mod filesystem;
pub mod i18n;
pub mod keymap;
pub mod logs_window;
pub mod mail;
pub mod material;
pub mod menu;
pub mod model;
pub mod open_files;
pub mod preview;
pub mod save;
pub mod settings;
pub mod settings_window;
pub mod sheet;
pub mod sidebar;
pub mod skin;
pub mod store;
pub mod stress;
pub mod theme;
pub mod toolbar;
pub mod trash_window;
pub mod updater;
pub mod window_registry;
