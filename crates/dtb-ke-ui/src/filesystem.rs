use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};

use chrono::{Datelike, Timelike};

const APPLICATION_IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzpläne";
const DATABASE_FILE: &str = "PersistedSessions.bin";

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
}
