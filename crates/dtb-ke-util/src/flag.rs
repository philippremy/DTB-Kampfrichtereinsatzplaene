//! Process-wide switches: how the app's on/off options are stored, in one shape each.
//!
//! * a plain runtime flag → [`std::sync::atomic::AtomicBool`] (no wrapper needed);
//! * a runtime flag that can also be "not set" (default / forced on / forced off) → [`AtomicOptBool`];
//! * a flag read once from an environment variable → [`EnvFlag`].
//!
//! All of them are meant to be module-level `static`s, readable from any thread. (`thread_local!` is for
//! main-thread-only handles, never for switches.)

use std::ffi::OsStr;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, Ordering};

/// An atomic `Option<bool>`: `None` = unset.
#[derive(Debug)]
pub struct AtomicOptBool(AtomicU8);

impl AtomicOptBool {
    pub const fn new() -> Self {
        Self(AtomicU8::new(0))
    }

    pub fn get(&self) -> Option<bool> {
        match self.0.load(Ordering::Relaxed) {
            1 => Some(false),
            2 => Some(true),
            _ => None,
        }
    }

    pub fn set(&self, value: Option<bool>) {
        self.0.store(
            match value {
                None => 0,
                Some(false) => 1,
                Some(true) => 2,
            },
            Ordering::Relaxed,
        );
    }
}

impl Default for AtomicOptBool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy)]
enum Parse {
    /// On iff the variable is set at all (any value, even empty).
    Present,
    /// `default` when unset; when set, off only for exactly `0`, `off` or `false`.
    OffTokens { default: bool },
    /// `default` when unset; when set, off for the empty string or `0`.
    Truthy { default: bool },
}

/// A boolean read from an environment variable, once, on first use.
pub struct EnvFlag {
    name: &'static str,
    parse: Parse,
    value: OnceLock<bool>,
}

impl EnvFlag {
    /// On iff `name` is set (to anything).
    pub const fn present(name: &'static str) -> Self {
        Self { name, parse: Parse::Present, value: OnceLock::new() }
    }

    /// `default` if unset, otherwise on unless the value is exactly `0`, `off` or `false`.
    pub const fn off_tokens(name: &'static str, default: bool) -> Self {
        Self { name, parse: Parse::OffTokens { default }, value: OnceLock::new() }
    }

    /// `default` if unset, otherwise off for the empty string or `0`, on for anything else.
    pub const fn truthy(name: &'static str, default: bool) -> Self {
        Self { name, parse: Parse::Truthy { default }, value: OnceLock::new() }
    }

    pub fn get(&self) -> bool {
        *self.value.get_or_init(|| self.read(std::env::var_os(self.name).as_deref()))
    }

    /// The parse rule on an already-fetched value (`None` = unset).
    fn read(&self, value: Option<&OsStr>) -> bool {
        // A value that isn't valid UTF-8 can't be one of the off tokens, so it counts as "something else".
        let text = value.map(|v| v.to_str().unwrap_or("\u{fffd}"));
        match (self.parse, text) {
            (Parse::Present, v) => v.is_some(),
            (Parse::OffTokens { default } | Parse::Truthy { default }, None) => default,
            (Parse::OffTokens { .. }, Some(v)) => !matches!(v, "0" | "off" | "false"),
            (Parse::Truthy { .. }, Some(v)) => !(v.is_empty() || v == "0"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn opt_bool_round_trips_all_three_states() {
        let f = AtomicOptBool::new();
        assert_eq!(f.get(), None);
        f.set(Some(true));
        assert_eq!(f.get(), Some(true));
        f.set(Some(false));
        assert_eq!(f.get(), Some(false));
        f.set(None);
        assert_eq!(f.get(), None);
    }

    #[test]
    fn parse_rules() {
        let off = EnvFlag::off_tokens("X", true);
        assert!(off.read(None));
        for v in ["0", "off", "false"] {
            assert!(!off.read(Some(OsStr::new(v))), "{v}");
        }
        for v in ["1", "on", "", "OFF"] {
            assert!(off.read(Some(OsStr::new(v))), "{v:?}");
        }

        let truthy = EnvFlag::truthy("X", true);
        assert!(truthy.read(None));
        assert!(!truthy.read(Some(OsStr::new(""))) && !truthy.read(Some(OsStr::new("0"))));
        assert!(truthy.read(Some(OsStr::new("1"))) && truthy.read(Some(OsStr::new("off"))));
        assert!(!EnvFlag::truthy("X", false).read(None));

        let present = EnvFlag::present("DTB_KE_TEST_DEFINITELY_UNSET_FLAG");
        assert!(!present.read(None));
        assert!(present.read(Some(OsStr::new(""))));
    }
}
