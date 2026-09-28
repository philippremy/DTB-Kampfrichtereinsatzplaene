//! Finding crashed runs at the next launch.

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Pending {
    pub crash: PathBuf,
    pub session: PathBuf,
}

/// `.crash` files in `dir` (each paired with its `.session` sidecar), oldest first. A crash file that
/// is empty or unreadable is deleted — the run died before the handler wrote anything useful.
pub fn find(dir: &Path) -> Vec<Pending> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut found: Vec<(std::time::SystemTime, Pending)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("crash") {
            continue;
        }
        let meta = entry.metadata();
        if meta.as_ref().map(|m| m.len()).unwrap_or(0) < 16 {
            let _ = std::fs::remove_file(&path);
            continue;
        }
        let modified = meta.and_then(|m| m.modified()).unwrap_or(std::time::UNIX_EPOCH);
        let session = path.with_extension("session");
        found.push((modified, Pending { crash: path, session }));
    }
    found.sort_by_key(|(t, _)| *t);
    found.into_iter().map(|(_, p)| p).collect()
}

/// Delete the sidecars of runs that ended normally (no matching `.crash`), except the running
/// process's own. Call once at startup, after [`find`].
pub fn remove_stale_sessions(dir: &Path, own_session: Option<&Path>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.flatten() {
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str());
        let stale = match ext {
            Some("session") => {
                own_session != Some(path.as_path()) && !path.with_extension("crash").exists()
            }
            Some("tmp") => true,
            _ => false,
        };
        if stale {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Remove a handled crash's files.
pub fn remove(p: &Pending) {
    let _ = std::fs::remove_file(&p.crash);
    let _ = std::fs::remove_file(&p.session);
}
