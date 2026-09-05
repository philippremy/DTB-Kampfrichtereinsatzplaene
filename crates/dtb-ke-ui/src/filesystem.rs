use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
    time::{Duration, SystemTime},
};

use chrono::{Datelike, Timelike};

const APPLICATION_IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzpläne";
const DATABASE_FILE: &str = "PersistedSessions.bin";

/// How long a rotated session log is kept before [`FilesystemHelper::gc_old_logs`]
/// removes it — nothing else ever cleans these up, and one accumulates per
/// session, so left alone they grow without the user ever noticing.
const LOG_RETENTION: Duration = Duration::from_secs(7 * 24 * 60 * 60);

static FS_HELPER: OnceLock<FilesystemHelper> = OnceLock::new();

/// A global filesystem helper: resolves and creates the per-OS data and log
/// directories for the application.
pub(crate) struct FilesystemHelper {
    data_dir: PathBuf,
    log_dir: PathBuf,
    log_file: PathBuf,
}

impl FilesystemHelper {
    /// Performs folder initialization.
    fn initialize(self) -> Self {
        // Create log and data folders if they do not exist
        std::fs::create_dir_all(&self.log_dir).expect("Creating the logs directory cannot fail");
        std::fs::create_dir_all(&self.data_dir).expect("Creating the data directory cannot fail");
        self
    }

    /// Returns the global instance of the filesystem helper.
    pub fn instance() -> &'static Self {
        FS_HELPER.get_or_init(|| {
            let data_dir = dirs::data_local_dir()
                .expect("data_local_dir is always defined")
                .join(APPLICATION_IDENTIFIER);
            let log_dir = cfg_select! {
                target_os = "macos" => {
                    dirs::data_local_dir()
                        .expect("data_local_dir is always defined")
                        .parent()
                        .expect("/Users/<USER>/Library always exists on macOS")
                        .join("Logs")
                        .join(APPLICATION_IDENTIFIER)
                }
                _ => {
                    data_dir
                        .join("Logs")
                }
            };
            let now = chrono::Local::now();
            let log_file = log_dir.join(format!(
                "DTB-KE_{}-{}-{}+{}{}{}.log",
                now.year(),
                now.month(),
                now.day(),
                now.hour(),
                now.minute(),
                now.second()
            ));
            Self {
                data_dir,
                log_dir,
                log_file,
            }
            .initialize()
        })
    }

    /// Returns the application log dir.
    pub fn get_log_dir(&'static self) -> &'static Path {
        self.log_dir.as_path()
    }

    /// Returns the application log file for the active session.
    pub fn get_log_file(&'static self) -> &'static Path {
        self.log_file.as_path()
    }

    /// Returns the application data dir.
    pub fn get_data_dir(&'static self) -> &'static Path {
        self.data_dir.as_path()
    }

    /// Returns the path to the persisted-sessions database file inside the data
    /// dir. The file itself is created lazily by `dtb_ke_persist::Db::open`.
    pub fn database_path(&'static self) -> PathBuf {
        self.data_dir.join(DATABASE_FILE)
    }

    /// Deletes every `*.log` file directly inside the log directory whose
    /// last-modified time is older than [`LOG_RETENTION`] (7 days). Called
    /// once at startup (`main`, right after the logger is registered) — a new
    /// session log is created every launch and nothing else ever removes an
    /// old one, so they'd otherwise accumulate silently forever.
    ///
    /// Never touches the active session's own file ([`Self::get_log_file`]),
    /// and never recurses — crash dumps live in a `crashes/` subdirectory
    /// (see `dtb_ke_crash::library::install`) and are handled on their own
    /// terms (kept until a report is actually sent), not swept by age here.
    /// Best-effort throughout: a directory that can't be read, or a single
    /// entry that can't be inspected or removed, is skipped rather than
    /// failing the whole pass — housekeeping must never block startup.
    /// Returns the number of files actually removed, for the caller to log.
    pub fn gc_old_logs(&'static self) -> usize {
        gc_logs_in(
            &self.log_dir,
            self.get_log_file(),
            LOG_RETENTION,
            SystemTime::now(),
        )
    }
}

/// The actual sweep behind [`FilesystemHelper::gc_old_logs`], factored out so
/// it's testable against a scratch directory instead of the real app log
/// dir. `now` is passed in (rather than read internally) so a test can
/// simulate time passing without touching any file's real mtime or actually
/// waiting. See that method's doc comment for the exact contract.
fn gc_logs_in(dir: &Path, current: &Path, retention: Duration, now: SystemTime) -> usize {
    let Some(cutoff) = now.checked_sub(retention) else {
        return 0; // system clock implausibly close to the epoch
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return 0;
    };

    let mut removed = 0;
    for entry in entries.flatten() {
        let path = entry.path();
        if path == current || path.extension().and_then(|e| e.to_str()) != Some("log") {
            continue;
        }
        let is_old = entry
            .metadata()
            .and_then(|meta| meta.modified())
            .is_ok_and(|modified| modified < cutoff);
        if is_old && std::fs::remove_file(&path).is_ok() {
            removed += 1;
        }
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scratch directory under the OS temp dir, cleaned up on drop —
    /// avoids touching the real app log directory and any dependency just
    /// for one test.
    struct ScratchDir(PathBuf);

    impl ScratchDir {
        fn new(name: &str) -> Self {
            let dir = std::env::temp_dir().join(format!(
                "dtb-ke-ui-test-{name}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            ));
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn file(&self, name: &str) -> PathBuf {
            let path = self.0.join(name);
            std::fs::write(&path, b"log line").unwrap();
            path
        }
    }

    impl Drop for ScratchDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    // No file's real mtime is ever manipulated below (no extra dependency
    // for it, and backdating mtimes is inherently a little flaky against
    // filesystems with coarse mtime resolution). Instead `now` is pushed
    // forward by more than the retention window, which — since every
    // scratch file's real mtime is "just now" — deterministically makes
    // them all look old *except* the ones a test explicitly wants to prove
    // survive on their own merits (the current-session guard, the .log
    // extension filter), all provable in one pass without waiting.

    #[test]
    fn old_files_are_removed_but_current_and_non_log_survive() {
        let dir = ScratchDir::new("gc-old");
        let old = dir.file("DTB-KE_old.log");
        let current = dir.file("DTB-KE_current.log");
        let non_log = dir.file("notes.txt");

        // Every file here was just written "now" — simulating a point 8
        // days later, with a 7-day retention, makes every one of them
        // (by mtime alone) fall outside the window.
        let eight_days = Duration::from_secs(8 * 24 * 60 * 60);
        let simulated_now = SystemTime::now() + eight_days;

        let removed = gc_logs_in(&dir.0, &current, LOG_RETENTION, simulated_now);

        assert_eq!(removed, 1);
        assert!(!old.exists(), "an old .log file must be removed");
        assert!(
            current.exists(),
            "the active session's own file must survive regardless of age"
        );
        assert!(non_log.exists(), "a non-.log file must never be touched");
    }

    #[test]
    fn a_file_inside_the_retention_window_survives() {
        let dir = ScratchDir::new("gc-recent");
        let recent = dir.file("DTB-KE_recent.log");

        // No simulated time passing at all — `recent`'s real mtime is well
        // inside the 7-day window measured from right now.
        let removed = gc_logs_in(
            &dir.0,
            Path::new("/nonexistent"),
            LOG_RETENTION,
            SystemTime::now(),
        );

        assert_eq!(removed, 0);
        assert!(recent.exists());
    }

    #[test]
    fn a_missing_directory_is_a_harmless_no_op() {
        let missing = std::env::temp_dir().join("dtb-ke-ui-test-gc-does-not-exist");
        let removed = gc_logs_in(
            &missing,
            &missing.join("current.log"),
            LOG_RETENTION,
            SystemTime::now(),
        );
        assert_eq!(removed, 0);
    }
}
