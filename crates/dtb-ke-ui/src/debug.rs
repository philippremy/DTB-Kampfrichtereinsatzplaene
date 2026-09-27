//! Hidden developer options — the settings window's "Debugging" tab (unlocked by clicking
//! "Erweitert" five times quickly, for the current session only).
//!
//! Two kinds of option:
//!
//! * **live** ones take effect at once through the statics in [`live`], kept in sync with
//!   [`DebugSettings`] by [`Settings::init`](crate::settings::Settings::init) /
//!   [`Settings::update`](crate::settings::Settings::update);
//! * **restart** ones are read once at startup by code that only looks at an environment variable
//!   (parts of them live in the vendored gpui / taffy). [`DebugSettings::apply_startup_env`] exports
//!   them as that variable, first thing in `main` before any other thread exists. A variable that is
//!   already set in the environment always wins over the stored option.
//!
//! [`crash`] holds the crash simulations, shared by the tab and the `DTB_KE_CRASH_TEST` env hook.

use gpui_kit::SharedString;
use serde::{Deserialize, Serialize};

use crate::i18n::{ActiveLocale, Locale};

/// A three-way switch: the built-in default, forced on, forced off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tri {
    #[default]
    Default,
    On,
    Off,
}

impl Tri {
    pub const ALL: [Self; 3] = [Self::Default, Self::On, Self::Off];

    pub fn label(self, locale: &Locale) -> SharedString {
        locale.t(match self {
            Self::Default => "settings.debug.tri-default",
            Self::On => "settings.debug.tri-on",
            Self::Off => "settings.debug.tri-off",
        })
    }

    fn code(self) -> u8 {
        match self {
            Self::Default => 0,
            Self::On => 1,
            Self::Off => 2,
        }
    }

    fn as_option(self) -> Option<bool> {
        match self {
            Self::Default => None,
            Self::On => Some(true),
            Self::Off => Some(false),
        }
    }
}

/// What `mail::send` does instead of talking to SMTP (`DTB_KE_MAIL_FAKE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MailSim {
    #[default]
    Off,
    Success,
    Failure,
}

impl MailSim {
    pub const ALL: [Self; 3] = [Self::Off, Self::Success, Self::Failure];

    pub fn label(self, locale: &Locale) -> SharedString {
        locale.t(match self {
            Self::Off => "settings.debug.mail-off",
            Self::Success => "settings.debug.mail-success",
            Self::Failure => "settings.debug.mail-failure",
        })
    }
}

/// Forces a skin (`DTB_KE_SKIN`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum SkinOverride {
    #[default]
    Auto,
    Mac,
    Win,
    Linux,
    Ipad,
}

impl SkinOverride {
    pub const ALL: [Self; 5] = [Self::Auto, Self::Mac, Self::Win, Self::Linux, Self::Ipad];

    pub fn label(self, locale: &Locale) -> SharedString {
        locale.t(match self {
            Self::Auto => "settings.debug.skin-auto",
            Self::Mac => "settings.debug.skin-mac",
            Self::Win => "settings.debug.skin-win",
            Self::Linux => "settings.debug.skin-linux",
            Self::Ipad => "settings.debug.skin-ipad",
        })
    }

    fn env_value(self) -> Option<&'static str> {
        match self {
            Self::Auto => None,
            Self::Mac => Some("mac"),
            Self::Win => Some("win"),
            Self::Linux => Some("linux"),
            Self::Ipad => Some("ipad"),
        }
    }
}

/// The persisted developer options (`[debug]` in `Settings.toml`). Everything defaults to "as shipped".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DebugSettings {
    // ── live ──
    /// The FPS overlay. `Default`: debug builds only.
    pub fps_hud: Tri,
    /// Simulated mail transport (an env `DTB_KE_MAIL_FAKE` wins).
    pub mail_sim: MailSim,
    /// Use the in-app export panel instead of the native save dialog.
    pub save_fallback: bool,
    /// Overrides the updater's manifest URL for both channels (empty = none).
    pub update_manifest: String,
    /// Log only our own crates' records. `Default`: release builds only.
    pub restrict_log_targets: Tri,
    // ── restart ──
    pub view_cache: bool,
    pub layout_retain: bool,
    pub layout_verify: bool,
    pub view_cache_verify: bool,
    /// One Taffy cache entry per slot, as upstream.
    pub taffy_single_way: bool,
    pub no_glass: bool,
    pub no_native: bool,
    pub skin: SkinOverride,
}

impl Default for DebugSettings {
    fn default() -> Self {
        Self {
            fps_hud: Tri::Default,
            mail_sim: MailSim::Off,
            save_fallback: false,
            update_manifest: String::new(),
            restrict_log_targets: Tri::Default,
            view_cache: true,
            layout_retain: true,
            layout_verify: false,
            view_cache_verify: false,
            taffy_single_way: false,
            no_glass: false,
            no_native: false,
            skin: SkinOverride::Auto,
        }
    }
}

impl DebugSettings {
    /// Export the restart-only options as the environment variables the rest of the app reads.
    ///
    /// Must run first thing in `main`, before the logger or any other thread exists (`set_var` is not
    /// thread-safe). A variable already present in the environment is left alone.
    pub fn apply_startup_env(&self) {
        let set = |name: &str, value: &str| {
            if std::env::var_os(name).is_none() {
                // SAFETY: called from `main` before any other thread is spawned.
                unsafe { std::env::set_var(name, value) };
            }
        };
        if !self.view_cache {
            set("DTB_KE_VIEW_CACHE", "0");
        }
        if !self.layout_retain {
            set("DTB_KE_LAYOUT_RETAIN", "0");
        }
        if self.layout_verify {
            set("DTB_KE_LAYOUT_VERIFY", "1");
        }
        if self.view_cache_verify {
            set("DTB_KE_VIEW_CACHE_VERIFY", "1");
        }
        if self.taffy_single_way {
            set("DTB_KE_TAFFY_CACHE_WAYS", "1");
        }
        if self.no_glass {
            set("DTB_KE_NO_GLASS", "1");
        }
        if self.no_native {
            set("DTB_KE_NO_NATIVE", "1");
        }
        if let Some(skin) = self.skin.env_value() {
            set("DTB_KE_SKIN", skin);
        }
    }
}

/// The live options, readable from any thread without a `&App` (the FPS HUD is checked every frame).
///
/// Every accessor here already applies the precedence rule — an environment variable that is set wins
/// over the stored developer option — so call sites just call the accessor.
pub mod live {
    use std::sync::RwLock;
    use std::sync::atomic::{AtomicBool, Ordering};

    use dtb_ke_util::flag::{AtomicOptBool, EnvFlag};

    use super::*;

    static FPS_HUD: AtomicOptBool = AtomicOptBool::new();
    static MAIL_SIM: AtomicOptBool = AtomicOptBool::new();
    static SAVE_FALLBACK: AtomicBool = AtomicBool::new(false);
    static UPDATE_MANIFEST: RwLock<String> = RwLock::new(String::new());
    static UNLOCKED: AtomicBool = AtomicBool::new(false);

    static ENV_FPS_HUD: EnvFlag = EnvFlag::truthy("DTB_KE_PERF_HUD", cfg!(debug_assertions));
    static ENV_SAVE_FALLBACK: EnvFlag = EnvFlag::present("DTB_KE_SAVE_FALLBACK");
    static ENV_DEBUG_TAB: EnvFlag = EnvFlag::present("DTB_KE_DEBUG_TAB");

    /// Push the current [`DebugSettings`] into the statics (and the logger).
    pub fn sync(debug: &DebugSettings) {
        FPS_HUD.set(debug.fps_hud.as_option());
        MAIL_SIM.set(match debug.mail_sim {
            MailSim::Off => None,
            MailSim::Success => Some(true),
            MailSim::Failure => Some(false),
        });
        SAVE_FALLBACK.store(debug.save_fallback, Ordering::Relaxed);
        if let Ok(mut manifest) = UPDATE_MANIFEST.write() {
            *manifest = debug.update_manifest.trim().to_string();
        }
        dtb_ke_log::set_restrict_targets(debug.restrict_log_targets.as_option());
    }

    /// Whether the FPS overlay renders: the developer option, else `DTB_KE_PERF_HUD`, else debug builds.
    pub fn fps_hud() -> bool {
        FPS_HUD.get().unwrap_or_else(|| ENV_FPS_HUD.get())
    }

    /// A simulated mail transport: `Some(true)` = succeed, `Some(false)` = fail, `None` = really send.
    /// `DTB_KE_MAIL_FAKE=ok|err` wins over the option.
    pub fn mail_sim() -> Option<bool> {
        match std::env::var("DTB_KE_MAIL_FAKE").as_deref() {
            Ok("ok") => Some(true),
            Ok("err") => Some(false),
            _ => MAIL_SIM.get(),
        }
    }

    /// Use the in-app export panel (`DTB_KE_SAVE_FALLBACK` or the option).
    pub fn save_fallback() -> bool {
        ENV_SAVE_FALLBACK.get() || SAVE_FALLBACK.load(Ordering::Relaxed)
    }

    /// The updater manifest override: `DTB_KE_UPDATE_MANIFEST`, else the option.
    pub fn update_manifest() -> Option<String> {
        std::env::var("DTB_KE_UPDATE_MANIFEST")
            .ok()
            .or_else(|| UPDATE_MANIFEST.read().ok().map(|m| m.clone()))
            .filter(|m| !m.is_empty())
    }

    /// Whether the hidden "Debugging" tab is unlocked in this session (`DTB_KE_DEBUG_TAB` unlocks it from
    /// the start — a test aid, since the gesture can't be scripted).
    pub fn unlocked() -> bool {
        UNLOCKED.load(Ordering::Relaxed) || ENV_DEBUG_TAB.get()
    }

    pub fn unlock() {
        UNLOCKED.store(true, Ordering::Relaxed);
    }

    /// `DTB_KE_DEBUG_TAB` also makes the settings window open on the Debugging tab.
    pub fn open_on_debug_tab() -> bool {
        ENV_DEBUG_TAB.get()
    }
}

/// Crash simulations. What can actually be raised, and how, is [`crate::fault::Fault`] — this
/// module is just the scheduling/marker plumbing around it (the countdown delay, which thread,
/// the `DTB_KE_CRASH_TEST` env hook, and marking a report as a simulation).
pub mod crash {
    use std::time::Duration;

    use gpui_kit::{App, AppContext};

    use crate::fault::Fault;

    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub enum Thread {
        Main,
        /// A worker thread with no name (the original `DTB_KE_CRASH_TEST` behaviour).
        Worker,
        /// A worker thread named `crash-test`.
        NamedWorker,
    }

    /// The crash directory (also where dumps and pending snapshots live).
    fn crash_dir() -> std::path::PathBuf {
        crate::filesystem::FilesystemHelper::instance().get_log_dir().join("crashes")
    }

    fn marker() -> std::path::PathBuf {
        crash_dir().join(".simulated")
    }

    /// Note that the coming crash is a simulation, so the reporter marks its e-mail `[Test]`.
    fn mark_simulated() {
        let dir = crash_dir();
        if let Err(err) = std::fs::create_dir_all(&dir).and_then(|_| std::fs::write(marker(), b"")) {
            log::warn!("crash simulation: cannot write the marker: {err}");
        }
    }

    /// Whether the last crash was a simulation (consumes the marker).
    pub fn take_simulated() -> bool {
        std::fs::remove_file(marker()).is_ok()
    }

    /// Prefix a report subject with `[Test]` if the crash it describes was simulated.
    pub fn mark_subject(subject: String) -> String {
        if take_simulated() { format!("[Test] {subject}") } else { subject }
    }

    /// Fault right here, right now.
    fn fault_now(fault: Fault) {
        eprintln!("crash simulation: faulting now ({fault:?})");
        mark_simulated();
        fault.trigger();
    }

    /// Crash the app on purpose after `delay`.
    pub fn trigger(fault: Fault, thread: Thread, delay: Duration, cx: &mut App) {
        log::warn!("crash simulation armed: {fault:?} on {thread:?} in {delay:?}");
        if fault == Fault::Borrow {
            // Needs a real entity mid-`update`, which only exists on the main thread's `App`.
            struct Dummy;
            let entity = cx.new(|_| Dummy);
            cx.spawn(async move |cx| {
                cx.background_executor().timer(delay).await;
                cx.update(|cx| {
                    eprintln!("crash simulation: faulting now (borrow)");
                    mark_simulated();
                    let reentrant = entity.clone();
                    entity.update(cx, move |_, cx| {
                        reentrant.read(cx);
                    });
                });
            })
            .detach();
            return;
        }
        match thread {
            Thread::Main => {
                cx.spawn(async move |cx| {
                    cx.background_executor().timer(delay).await;
                    cx.update(|_| fault_now(fault));
                })
                .detach();
            }
            Thread::Worker | Thread::NamedWorker => {
                let run = move || {
                    std::thread::sleep(delay);
                    fault_now(fault);
                };
                let named = thread == Thread::NamedWorker;
                let spawned = if named {
                    std::thread::Builder::new().name("crash-test".into()).spawn(run).map(drop)
                } else {
                    std::thread::Builder::new().spawn(run).map(drop)
                };
                if let Err(err) = spawned {
                    log::error!("crash simulation: cannot spawn the thread: {err}");
                }
            }
        }
    }

    /// `DTB_KE_CRASH_TEST=segv|bus|ill|trap|fpe|abort|sys|xcpu|xfsz|emt|pipe|panic|borrow|overflow|nsexception[,thread|,main]`
    /// — faults 2 s after launch, on an unnamed worker thread by default (a named one with
    /// `,thread`, the main thread with `,main`). Not gated by [`Fault::available`] — an
    /// unavailable-on-this-platform choice falls back to its own module-level `std::process::abort()`
    /// safety net (see `crate::fault`) rather than silently no-opping.
    pub fn from_env(cx: &mut App) {
        let Ok(spec) = std::env::var("DTB_KE_CRASH_TEST") else { return };
        let fault = match spec.split(',').next() {
            Some("bus") => Fault::Bus,
            Some("ill") => Fault::Ill,
            Some("trap") => Fault::Trap,
            Some("fpe") => Fault::Fpe,
            Some("abort") => Fault::Abort,
            Some("sys") => Fault::Sys,
            Some("xcpu") => Fault::Xcpu,
            Some("xfsz") => Fault::Xfsz,
            Some("emt") => Fault::Emt,
            Some("pipe") => Fault::Pipe,
            Some("panic") => Fault::Panic,
            Some("borrow") => Fault::Borrow,
            Some("overflow") => Fault::StackOverflow,
            Some("nsexception") => Fault::NSException,
            _ => Fault::Segv,
        };
        let thread = if spec.contains(",main") {
            Thread::Main
        } else if spec.contains(",thread") {
            Thread::NamedWorker
        } else {
            Thread::Worker
        };
        trigger(fault, thread, Duration::from_secs(2), cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_match_the_shipped_behaviour() {
        let d = DebugSettings::default();
        assert!(d.view_cache && d.layout_retain);
        assert!(!d.layout_verify && !d.view_cache_verify && !d.taffy_single_way);
        assert_eq!((d.fps_hud, d.mail_sim, d.skin), (Tri::Default, MailSim::Off, SkinOverride::Auto));
    }

    #[test]
    fn a_missing_section_and_partial_sections_fall_back_to_defaults() {
        let d: DebugSettings = toml::from_str("").unwrap();
        assert_eq!(d, DebugSettings::default());
        let d: DebugSettings = toml::from_str("fps_hud = \"on\"\nlayout_retain = false").unwrap();
        assert_eq!(d.fps_hud, Tri::On);
        assert!(!d.layout_retain && d.view_cache);
    }

    #[test]
    fn round_trips_through_toml() {
        let d = DebugSettings { mail_sim: MailSim::Failure, skin: SkinOverride::Ipad, update_manifest: "http://x/m.json".into(), ..Default::default() };
        let text = toml::to_string(&d).unwrap();
        assert_eq!(toml::from_str::<DebugSettings>(&text).unwrap(), d);
    }

    /// Every label text resolves to real text — a missing key would show as the key itself.
    /// [`crate::fault::Fault`]'s own labels/descriptions/unavailable-reasons are covered by
    /// `fault::tests::labels_are_all_in_the_catalog`.
    #[test]
    fn debug_labels_are_all_in_the_catalog() {
        let locale = crate::i18n::test_locale();
        let looks_like_key = |t: gpui_kit::SharedString| t.starts_with("settings.debug.");
        for t in Tri::ALL {
            assert!(!looks_like_key(t.label(&locale)), "{t:?}");
        }
        for m in MailSim::ALL {
            assert!(!looks_like_key(m.label(&locale)), "{m:?}");
        }
        for k in SkinOverride::ALL {
            assert!(!looks_like_key(k.label(&locale)), "{k:?}");
        }
    }

    #[test]
    fn live_statics_follow_the_settings() {
        live::sync(&DebugSettings { fps_hud: Tri::Off, mail_sim: MailSim::Success, save_fallback: true, update_manifest: " u ".into(), ..Default::default() });
        assert!(!live::fps_hud());
        assert_eq!(live::mail_sim(), Some(true));
        assert!(live::save_fallback());
        assert_eq!(live::update_manifest().as_deref(), Some("u"));
        live::sync(&DebugSettings::default());
        assert_eq!(live::fps_hud(), cfg!(debug_assertions));
        assert_eq!(live::mail_sim(), None);
        assert!(!live::save_fallback());
        assert_eq!(live::update_manifest(), None);
    }
}
