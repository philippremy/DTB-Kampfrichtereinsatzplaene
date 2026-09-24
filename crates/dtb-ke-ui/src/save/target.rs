//! Where an exported file ends up, and how a file the user picked reaches the app.
//!
//! Desktop save panels return a path first and the app then writes to it ([`Target::Path`]). On
//! iPadOS the user's folders are outside the app's sandbox, so `std::fs` cannot write to them and
//! there is no save panel that returns a path: the app writes the file into its own temporary
//! directory first and the system document picker then copies it to wherever the user chooses
//! ([`Target::Deferred`], see `gpui_ios::ios::documents`). Callers use one sequence for both:
//!
//! 1. [`Target::staging_path`] — where to write;
//! 2. write the file there;
//! 3. [`Target::deliver`] — a no-op for a chosen path, the picker for a deferred target.
//!
//! Imports are the mirror image: [`prompt_import`] resolves to paths the app can read with plain
//! `std::fs` (on iPadOS the picker hands back local copies).

use std::path::{Path, PathBuf};

use futures::channel::oneshot;
use gpui_kit::{App, SharedString};

/// Where an export is going.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Target {
    /// A path the user already chose: write straight to it.
    Path(PathBuf),
    /// No path yet (iPadOS): stage the file, then let the user pick a destination.
    Deferred { file_name: String },
}

/// How a delivery ended.
#[derive(Debug)]
pub enum Delivery {
    /// The file is where the user wanted it (the path, when the platform reports it).
    Saved(Option<PathBuf>),
    /// The user backed out of the destination picker.
    Cancelled,
}

impl Target {
    /// The file to write. For a deferred target: a fresh directory under the temporary directory,
    /// so that two exports never collide and the user sees `file_name` in the picker.
    pub fn staging_path(&self) -> std::io::Result<PathBuf> {
        match self {
            Target::Path(path) => Ok(path.clone()),
            Target::Deferred { file_name } => {
                let directory =
                    std::env::temp_dir().join(format!("{STAGING_PREFIX}{}", uuid::Uuid::new_v4()));
                std::fs::create_dir_all(&directory)?;
                Ok(directory.join(sanitize_file_name(file_name)))
            }
        }
    }

    /// Finish an export whose file has been fully written to `staged`. The staging file is removed
    /// afterwards, whatever the outcome.
    pub async fn deliver(self, staged: &Path) -> Result<Delivery, String> {
        match self {
            Target::Path(path) => Ok(Delivery::Saved(Some(path))),
            Target::Deferred { .. } => {
                let outcome = deliver_deferred(staged).await;
                self.discard(staged);
                outcome
            }
        }
    }

    /// Remove the staging file of a deferred target whose export failed before delivery.
    pub fn discard(&self, staged: &Path) {
        if matches!(self, Target::Deferred { .. }) {
            if let Err(err) = std::fs::remove_file(staged) {
                log::debug!("could not remove the staged export {}: {err}", staged.display());
            }
            if let Some(directory) = staged.parent() {
                // Only ever the directory `staging_path` made; fails harmlessly if not empty.
                std::fs::remove_dir(directory).ok();
            }
        }
    }
}

/// Prefix of the per-export staging directories under the temporary directory.
const STAGING_PREFIX: &str = "dtbke-export-";

/// Remove staging directories a previous run left behind (killed while the destination picker was
/// open, say). Call once at startup, before any export can be staging.
pub fn remove_stale_staging() {
    let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) else {
        return;
    };
    for entry in entries.flatten() {
        if entry.file_name().to_string_lossy().starts_with(STAGING_PREFIX) {
            if let Err(err) = std::fs::remove_dir_all(entry.path()) {
                log::debug!("could not remove stale staging {}: {err}", entry.path().display());
            }
        }
    }
}

/// A file name the system's file providers accept.
fn sanitize_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') || c.is_control() { '_' } else { c })
        .collect();
    let cleaned = cleaned.trim().trim_start_matches('.').to_owned();
    if cleaned.is_empty() { "Export".to_owned() } else { cleaned }
}

#[cfg(target_os = "ios")]
async fn deliver_deferred(staged: &Path) -> Result<Delivery, String> {
    match gpui_ios::ios::documents::export_files(&[staged.to_path_buf()]).await {
        Ok(Ok(Some(destinations))) => Ok(Delivery::Saved(destinations.into_iter().next())),
        Ok(Ok(None)) => Ok(Delivery::Cancelled),
        Ok(Err(err)) => Err(err.to_string()),
        Err(_) => Err("the document picker went away".to_owned()),
    }
}

#[cfg(not(target_os = "ios"))]
async fn deliver_deferred(_staged: &Path) -> Result<Delivery, String> {
    Err("deferred delivery only exists on iPadOS".to_owned())
}

/// Ask the user for a destination for a backup file with no format options (the whole-database
/// copy). Resolves to `None` when they cancel.
pub fn prompt_backup_target(file_name: &str, cx: &mut App) -> oneshot::Receiver<Option<Target>> {
    let (sender, receiver) = oneshot::channel();
    if cfg!(target_os = "ios") {
        let target = Target::Deferred {
            file_name: file_name.to_owned(),
        };
        if sender.send(Some(target)).is_err() {
            log::debug!("backup target receiver was dropped");
        }
        return receiver;
    }
    let dialog = cx.prompt_for_new_path(&super::default_dir(), Some(file_name));
    cx.spawn(async move |_| {
        let target = match dialog.await {
            Ok(Ok(Some(path))) => Some(Target::Path(path)),
            Ok(Ok(None)) | Err(_) => None,
            Ok(Err(err)) => {
                log::warn!("the save dialog failed: {err}");
                None
            }
        };
        if sender.send(target).is_err() {
            log::debug!("backup target receiver was dropped");
        }
    })
    .detach();
    receiver
}

/// Delete `path` after a successful read if the platform handed it over as a temporary copy (the
/// iPadOS document picker does; desktop dialogs return the user's own file, which is never touched).
pub fn discard_imported_copy(path: &Path) {
    if cfg!(target_os = "ios") && path.starts_with(std::env::temp_dir()) {
        if let Err(err) = std::fs::remove_file(path) {
            log::debug!("could not remove the temporary import copy {}: {err}", path.display());
        }
    }
}

/// Ask the user for a single `.dtbke` file to import. Resolves to `None` when they cancel, else to
/// paths readable with `std::fs`.
pub fn prompt_import(prompt: SharedString, cx: &mut App) -> oneshot::Receiver<Option<Vec<PathBuf>>> {
    let (sender, receiver) = oneshot::channel();
    #[cfg(target_os = "ios")]
    let dialog = {
        let _ = prompt;
        gpui_ios::ios::documents::pick_files(&[super::FormatKind::Blob.extension()], false)
    };
    #[cfg(not(target_os = "ios"))]
    let dialog = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
        files: true,
        directories: false,
        multiple: false,
        prompt: Some(prompt),
    });
    cx.spawn(async move |_| {
        let paths = match dialog.await {
            Ok(Ok(paths)) => paths,
            Ok(Err(err)) => {
                log::warn!("the open dialog failed: {err}");
                None
            }
            Err(_) => None,
        };
        if sender.send(paths).is_err() {
            log::debug!("import receiver was dropped");
        }
    })
    .detach();
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_names_are_made_safe_for_file_providers() {
        assert_eq!(sanitize_file_name("Wettkampf: A/B?.pdf"), "Wettkampf_ A_B_.pdf");
        assert_eq!(sanitize_file_name("  .hidden "), "hidden");
        assert_eq!(sanitize_file_name(""), "Export");
        assert_eq!(sanitize_file_name("Kampfrichtereinsatzpläne.dtbke"), "Kampfrichtereinsatzpläne.dtbke");
    }

    #[test]
    fn a_chosen_path_stages_in_place_and_deferred_gets_its_own_directory() {
        let path = Target::Path(PathBuf::from("/tmp/x.pdf"));
        assert_eq!(path.staging_path().unwrap(), PathBuf::from("/tmp/x.pdf"));

        let deferred = Target::Deferred {
            file_name: "Plan.pdf".into(),
        };
        let a = deferred.staging_path().unwrap();
        let b = deferred.staging_path().unwrap();
        assert_ne!(a.parent(), b.parent());
        assert_eq!(a.file_name().unwrap(), "Plan.pdf");
        deferred.discard(&a);
        deferred.discard(&b);
        assert!(!a.exists() && !a.parent().unwrap().exists());
    }
}
