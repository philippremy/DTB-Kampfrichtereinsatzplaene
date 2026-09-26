//! The on-disk archive: `<root>/<DEBUG_ID>/debug-file` (+ `meta.json`), one verified file per id.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Breakpad debug ids: 32 hex digits (GUID) followed by 1–8 hex digits (age).
pub fn normalise_id(raw: &str) -> Option<String> {
    let ok = (33..=40).contains(&raw.len()) && raw.bytes().all(|b| b.is_ascii_hexdigit());
    ok.then(|| raw.to_ascii_uppercase())
}

/// What the uploader said about a file (hints only — the id alone identifies it).
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Meta {
    pub name: Option<String>,
    pub commit: Option<String>,
    pub target: Option<String>,
    pub size: u64,
    pub uploaded_unix: u64,
}

pub struct Store {
    root: PathBuf,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

impl Store {
    /// Creates the store, and clears the leftovers of an interrupted upload.
    pub fn open(root: &Path) -> std::io::Result<Self> {
        std::fs::create_dir_all(root)?;
        let tmp = root.join(".tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp)?;
        Ok(Self {
            root: root.to_owned(),
        })
    }

    pub fn file_path(&self, id: &str) -> PathBuf {
        self.root.join(id).join("debug-file")
    }

    /// A fresh scratch path on the same filesystem as the archive (so the final rename is atomic).
    pub fn scratch_path(&self) -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::Relaxed);
        self.root
            .join(".tmp")
            .join(format!("upload-{}-{n}", std::process::id()))
    }

    /// Moves a verified scratch file into place. Returns whether it replaced an existing entry.
    pub fn commit(&self, id: &str, scratch: &Path, meta: &Meta) -> std::io::Result<bool> {
        let target = self.file_path(id);
        let dir = target.parent().expect("has a parent");
        std::fs::create_dir_all(dir)?;
        let replaced = target.exists();
        std::fs::write(dir.join("meta.json"), serde_json::to_vec_pretty(meta)?)?;
        std::fs::rename(scratch, &target)?;
        Ok(replaced)
    }
}

#[cfg(test)]
mod tests {
    use super::normalise_id;

    #[test]
    fn ids_are_strict_hex_of_the_right_length() {
        assert_eq!(
            normalise_id("0123456789abcdef0123456789ABCDEF1").as_deref(),
            Some("0123456789ABCDEF0123456789ABCDEF1")
        );
        assert!(normalise_id("").is_none());
        assert!(
            normalise_id("0123456789ABCDEF0123456789ABCDEF").is_none(),
            "no age"
        );
        assert!(normalise_id("../../etc/passwd0123456789ABCDEF012345").is_none());
        assert!(normalise_id("0123456789ABCDEF0123456789ABCDEFG").is_none());
        assert!(normalise_id(&"A".repeat(41)).is_none());
    }
}
