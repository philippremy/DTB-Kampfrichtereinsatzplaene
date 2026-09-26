//! In-process crash capture — the *thin* half.
//!
//! [`library::install`] registers an OS exception handler (Mach exception port
//! on macOS) + a panic hook. On a fault the handler thread does almost nothing:
//! it `fork`/`exec`s an embedded helper and hands it the crashed task's rights,
//! then waits for the helper to say it has read what it needs and exits. **All
//! the dangerous work — reading thread state, walking the stack + module list,
//! writing the dump — happens in the helper** (`dtb-ke-crashhandler`), a clean
//! process, via the `minidump-writer` crate.
//!
//! The dump is a standard **minidump** (`.dmp`): `lldb -c` / WinDbg / Visual
//! Studio open it directly, `minidump-stackwalk` and our own `dtb-ke-symbolize`
//! resolve it offline against the matching `.dSYM` / `.pdb` / DWARF.
//!
//! macOS, Windows and Linux are wired up; other targets get the panic hook only.

/// The filename the embedded helper is extracted to under the OS temp dir.
/// Shared so the crash reporter can recognise its parent process by name.
pub const HELPER_FILE_NAME: &str = if cfg!(windows) {
    "DTB-KE-Crashhandler.exe"
} else {
    "DTB-KE-Crashhandler"
};

/// Exit codes the **crash reporter** (a re-launched instance of the app that
/// recognises the helper as its parent) returns to the **helper**, which
/// `wait()`s for it. Only [`SENT`](report::SENT) lets the helper delete the
/// on-disk dump; every other code — and any abnormal exit — means "keep it".
pub mod report {
    /// The report was transmitted successfully — the helper deletes the dump.
    pub const SENT: i32 = 0;
    /// The user chose not to send — the helper keeps the dump on disk.
    pub const DECLINED: i32 = 10;
    /// The user chose to send, but transmission is deferred — keep for later.
    pub const QUEUED: i32 = 11;
    /// The reporter could not run (unreadable stdin, GUI init failed) — keep.
    pub const ERROR: i32 = 12;
}

#[cfg(feature = "snapshot")]
pub mod snapshot;

/// The build-info user stream: format, parser and the crash-time global.
pub mod buildinfo;

/// System-symbol hints: names for OS-library frames, computed on the crashed machine (a second user stream).
pub mod syshints;

/// Append a stream to a finished minidump (the helper adds [`buildinfo::STREAM_TYPE`] this way).
#[cfg(feature = "snapshot")]
pub mod patch;

/// Signal-safe crash-record writer + launch-time session snapshot (Apple arm64: iOS handler, macOS tests).
#[cfg(all(feature = "snapshot", any(target_os = "macos", target_os = "ios"), target_arch = "aarch64"))]
#[doc(hidden)]
pub mod apple;

#[cfg(feature = "as-library")]
mod panic;

/// The (in)famous coffin — logged once per fault via the signal-safe
/// [`dtb_ke_log::fault!`], right after a platform handler takes a crash. Each
/// line is its own `[F]` record (kernel-`dev_err!`-oops style).
#[cfg(feature = "as-library")]
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

#[cfg(all(feature = "as-library", target_os = "macos"))]
mod macos;

#[cfg(all(feature = "as-library", target_os = "ios"))]
pub mod ios;

#[cfg(all(feature = "as-library", target_os = "windows"))]
mod windows;

#[cfg(all(feature = "as-library", target_os = "linux"))]
mod linux;

#[cfg(feature = "as-library")]
pub mod library {

    use std::path::PathBuf;
    use std::sync::OnceLock;

    #[derive(Debug, Clone)]
    pub struct Config {
        /// Directory `.dmp` files are written to (created if missing). The helper
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
        /// The embedded helper is a 0-byte placeholder (`cargo dtb-ke-bundle` was
        /// not run) — capture is disabled, the panic hook is still installed.
        NoHelper,
        Os(String),
    }

    impl std::fmt::Display for InstallError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                InstallError::AlreadyInstalled => write!(f, "crash handler already installed"),
                InstallError::NoHelper => {
                    write!(f, "crash helper missing (run `cargo dtb-ke-bundle build`)")
                }
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
