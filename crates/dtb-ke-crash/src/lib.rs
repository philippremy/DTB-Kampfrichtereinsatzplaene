//! In-process crash capture.
//!
//! [`library::install`] registers an OS exception handler (Mach exception port on macOS) + a panic
//! hook. On a fault the handler thread does almost nothing: it `fork`/`exec`s (`execve`/
//! `CreateProcessW`) **this same executable, relaunched** with [`RELAUNCH_ARG`] as its first
//! argument — no separate helper binary, nothing to extract or embed — to do the real work: reading
//! thread state, walking the stack + module list, writing the dump (the `writer` module, via the
//! `minidump-writer` crate) against the crashed process from outside. `dtb-ke-ui`'s `main()`
//! recognises the flag and dispatches straight to the matching `crash_report::run_{macos,windows,
//! linux}_capture`, which captures the dump (`writer::{macos,windows,linux}::capture` +
//! `writer::build_report`, the platform-neutral digesting step) and shows the reporter dialog
//! itself, in the same process, before exiting with the user's verdict.
//!
//! iOS is the one genuine exception, not a migration-in-progress gap: it cannot spawn *any* process,
//! so it captures fully in-process (`apple`/`ios` modules, signal-safe, no relaunch at all) and defers
//! showing the reporter to the *next* real launch — see those modules' doc comments.
//!
//! The dump is a standard **minidump** (`.dtbkedmp`): `lldb -c` / WinDbg / Visual Studio open it
//! directly, `minidump-stackwalk` or `dtb-ke-debugger` resolve it offline against the matching
//! `.dSYM` / `.pdb` / DWARF.
//!
//! macOS, Windows and Linux are wired up; other targets get the panic hook only. No Cargo features:
//! every module here is small enough, and universally needed by every real consumer once you follow
//! the dependency graph (see the crate's own `Cargo.toml`), that gating parts of it behind a feature
//! flag bought no real isolation — `dtb-ke-debugger`/`dtb-ke-syms` never enabled it explicitly, but
//! always ended up with it anyway via `dtb-ke-ui` in the same build graph. Platform gating
//! (`target_os`) still does real work and stays.

/// The file extension (no dot) of a crash report — a standard minidump under a name of our own, so the
/// debugger (`dtb-ke-debugger`) can register as its handler. Every writer and reader uses this one constant.
pub const DUMP_EXTENSION: &str = "dtbkedmp";

/// `argv[1]` a self-relaunched instance carries to identify itself as the crash-time capture +
/// report process, rather than a normal launch. Every platform `execve`s/`CreateProcessW`s the app's
/// own binary with this as the first argument; `dtb-ke-ui`'s `main()` checks for it before doing
/// anything else that could fault (logging, settings, `library::install` itself).
pub const RELAUNCH_ARG: &str = "--dtb-ke-crash-reporter";

/// Exit codes the **crash reporter** — this same executable, relaunched (see the module doc comment)
/// — uses when it acts on the user's verdict: only [`SENT`] means the dump is gone (deleted by the
/// reporter itself, which owns the dump's whole lifecycle); every other code — and any abnormal exit
/// — means it's still on disk.
pub mod report {
    /// The report was transmitted successfully — the dump is deleted.
    pub const SENT: i32 = 0;
    /// The user chose not to send — the dump stays on disk.
    pub const DECLINED: i32 = 10;
    /// The user chose to send, but transmission is deferred — keep for later.
    pub const QUEUED: i32 = 11;
    /// The reporter could not run (unreadable stdin, GUI init failed) — keep.
    pub const ERROR: i32 = 12;
}

pub mod snapshot;

/// The build-info user stream: format, parser and the crash-time global.
pub mod buildinfo;

/// System-symbol hints: names for OS-library frames, computed on the crashed machine (a second user stream).
pub mod syshints;

/// An uncaught `NSException`'s name/reason/call-stack — macOS only (a third user stream).
pub mod nsexception;

/// Append a stream to a finished minidump (the helper adds [`buildinfo::STREAM_TYPE`] this way).
pub mod patch;

/// Out-of-process dump construction — reads the crashed process's memory and builds a minidump.
/// Heavy: pulls in `minidump-writer`/`crash-context`/`futures`. Every self-relaunched platform
/// (everywhere except iOS, which captures fully in-process instead) calls straight into this.
#[cfg(not(target_os = "ios"))]
pub mod writer;

/// Signal-safe crash-record writer + launch-time session snapshot (Apple arm64: iOS handler, macOS tests).
#[cfg(all(any(target_os = "macos", target_os = "ios"), target_arch = "aarch64"))]
#[doc(hidden)]
pub mod apple;

mod panic;

/// The (in)famous coffin — logged once per fault via the signal-safe
/// [`dtb_ke_log::fault!`], right after a platform handler takes a crash. Each
/// line is its own `[F]` record (kernel-`dev_err!`-oops style).
pub(crate) fn oops_art() {
    for line in [
        r" (\________/) ",
        r"  |        |  ",
        r"'.| \  , / |.'",
        r"--| / (( \ |--",
        r".'|  _-_-  |'.",
        r"  |________|  ",
    ] {
        dtb_ke_log::fault!("{line}");
    }
}

#[cfg(target_os = "macos")]
mod macos;

/// Test hook (the hidden Debugging tab / `DTB_KE_CRASH_TEST=nsexception`): raises a real, deliberately
/// unhandled `NSException` on the calling thread — see `macos::simulate_uncaught_nsexception`'s doc
/// comment for exactly what it exercises depending on which thread it's called from.
#[cfg(target_os = "macos")]
pub fn simulate_uncaught_nsexception() {
    macos::simulate_uncaught_nsexception();
}

#[cfg(target_os = "ios")]
pub mod ios;

#[cfg(target_os = "windows")]
mod windows;

#[cfg(target_os = "linux")]
mod linux;

pub mod library {

    use std::path::PathBuf;
    use std::sync::OnceLock;

    #[derive(Debug, Clone)]
    pub struct Config {
        /// Directory `.dtbkedmp` files are written to (created if missing). `writer::build_report`
        /// generates the filename.
        pub dump_dir: PathBuf,
        /// Short filename slug, e.g. `"DTB-KE"`.
        pub app_slug: String,
        /// The build-info payload (`buildinfo::render`) written into every dump as a user stream.
        pub build_info: &'static str,
    }

    #[derive(Debug)]
    pub enum InstallError {
        AlreadyInstalled,
        Os(String),
    }

    impl std::fmt::Display for InstallError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                InstallError::AlreadyInstalled => write!(f, "crash handler already installed"),
                InstallError::Os(e) => write!(f, "OS registration failed: {e}"),
            }
        }
    }
    impl std::error::Error for InstallError {}

    static CONFIG: OnceLock<Config> = OnceLock::new();

    /// Install the crash handler + panic hook. Call once, first thing in `main`.
    pub fn install(config: Config) -> Result<(), InstallError> {
        let _ = std::fs::create_dir_all(&config.dump_dir);
        CONFIG
            .set(config.clone())
            .map_err(|_| InstallError::AlreadyInstalled)?;

        super::panic::install_hook();
        super::buildinfo::set(config.build_info);

        #[cfg(target_os = "macos")]
        {
            super::macos::install(&config.dump_dir, &config.app_slug)
        }
        #[cfg(target_os = "ios")]
        {
            super::ios::install(&config.dump_dir, &config.app_slug)
        }
        #[cfg(target_os = "windows")]
        {
            super::windows::install(&config.dump_dir, &config.app_slug)
        }
        #[cfg(target_os = "linux")]
        {
            super::linux::install(&config.dump_dir, &config.app_slug)
        }
        #[cfg(not(any(target_os = "macos", target_os = "ios", target_os = "windows", target_os = "linux")))]
        {
            Ok(())
        }
    }
}
