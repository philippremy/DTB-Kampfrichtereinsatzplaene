//! The crashed build's identity, read from the build-info user stream.

use std::collections::BTreeMap;

use dtb_ke_crash::buildinfo::{STREAM_TYPE, key, parse};
use minidump::Minidump;

/// Everything the app wrote about itself (`build_info::stream_text` in `dtb-ke-ui`). Keys are kept
/// verbatim so a newer app's extra keys show up in the viewer without a code change here.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct BuildInfo {
    pub pairs: BTreeMap<String, String>,
}

impl BuildInfo {
    /// `None` when the dump has no build-info stream (an older build, or a foreign dump).
    pub fn from_dump<'a, T: std::ops::Deref<Target = [u8]> + 'a>(
        dump: &Minidump<'a, T>,
    ) -> Option<Self> {
        let raw = dump.get_raw_stream(STREAM_TYPE).ok()?;
        Some(Self::from_text(&String::from_utf8_lossy(raw)))
    }

    pub fn from_text(text: &str) -> Self {
        Self {
            pairs: parse(text)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect(),
        }
    }

    pub fn get(&self, key: &str) -> Option<&str> {
        self.pairs
            .get(key)
            .map(String::as_str)
            .filter(|v| !v.is_empty())
    }

    pub fn app_version(&self) -> Option<&str> {
        self.get(key::APP_VERSION)
    }

    /// The full commit hash when known, else the abbreviated one.
    pub fn commit(&self) -> Option<&str> {
        self.get("commit_full").or_else(|| self.get(key::COMMIT))
    }

    pub fn target(&self) -> Option<&str> {
        self.get(key::TARGET)
    }

    /// Absolute path of the workspace on the build machine — debug info's file paths start with it.
    pub fn workspace_root(&self) -> Option<&str> {
        self.get(key::WORKSPACE_ROOT)
    }

    /// The running executable's path on the crashed machine.
    pub fn exe_path(&self) -> Option<&str> {
        self.get("exe_path")
    }

    /// The app's repository URL, when the dump names it (`repository=` in the build-info stream).
    pub fn repository(&self) -> Option<&str> {
        self.get("repository")
    }

    /// The full 40-digit commit — what a fetch needs (an abbreviated one cannot be requested from a server).
    pub fn commit_full(&self) -> Option<&str> {
        self.get("commit_full")
            .filter(|c| c.len() == 40 && c.bytes().all(|b| b.is_ascii_hexdigit()))
    }

    /// The build had uncommitted changes, so its commit does not describe its source exactly.
    pub fn dirty(&self) -> bool {
        self.get("dirty") == Some("true")
    }
}
