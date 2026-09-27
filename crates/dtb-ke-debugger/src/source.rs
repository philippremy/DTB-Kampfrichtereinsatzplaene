//! Where debug files come from. A source answers one question: *do you have a file whose debug id is
//! this module's?* — never "a file with this name", so a stale build can never be mistaken for the
//! right one. New origins (the symbol server, a cache) implement the same trait.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use samply_symbols::debugid::DebugId;

use crate::build::BuildInfo;
use crate::identity::{FileIdentity, ModuleRef, identify};

/// A located file that matches the module's debug id.
#[derive(Clone, Debug)]
pub struct FoundFile {
    pub path: PathBuf,
    pub has_debug_info: bool,
    /// Defines named function symbols (a stripped binary does not — its frames only get synthesized `fun_<address>` names).
    pub has_symbols: bool,
    /// Human-readable origin for the UI (`"the executable itself"`, `"~/dsyms"`, `"symbol server"`).
    pub origin: String,
    /// The image lives inside this Mac's dyld shared cache (no file to load): `path` is only its install name.
    pub in_dyld_cache: bool,
}

#[async_trait]
pub trait DebugFileSource: Send + Sync {
    fn name(&self) -> String;

    /// `Ok(None)` = "not here"; `Err` = the source itself failed (network, unreadable dir) — the
    /// resolver records it and moves on to the next source.
    /// `build` is the crashed build's identity when the dump had one — routing information for sources
    /// that ask a server (the debug id alone already pins the exact file).
    async fn find(
        &self,
        module: &ModuleRef,
        build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>>;
}

/// The module's own code file, if it still exists on this machine and is the right build. Covers a
/// developer crashing a local (debug) build, whose executable carries — or points at — its DWARF.
pub struct ExecutableSource;

#[async_trait]
impl DebugFileSource for ExecutableSource {
    fn name(&self) -> String {
        "the module's own file".into()
    }

    async fn find(
        &self,
        module: &ModuleRef,
        _build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>> {
        let (Some(want), path) = (module.debug_id, module.code_path()) else {
            return Ok(None);
        };
        let found = tokio::task::spawn_blocking(move || {
            identify(&path)
                .into_iter()
                .find(|i| i.debug_id == want)
                .map(|i| (path, i))
        })
        .await?;
        Ok(found.map(|(path, id)| FoundFile {
            path,
            has_debug_info: id.has_debug_info,
            has_symbols: id.has_symbols,
            origin: "the executable itself".into(),
            in_dyld_cache: false,
        }))
    }
}

/// Files and directories the developer points the debugger at (a CI `debug-info` archive unpacked
/// anywhere, a `.dSYM` dropped in). Scanned once, lazily, into a debug-id index; nested `.dSYM`
/// bundles, `.pdb`s, `.dwp`s and bare binaries are all picked up by content, not by name.
pub struct DirectorySource {
    inner: std::sync::Arc<DirInner>,
}

struct DirInner {
    roots: Mutex<Vec<PathBuf>>,
    index: Mutex<Option<HashMap<DebugId, Vec<Entry>>>>,
}

#[derive(Clone)]
struct Entry {
    path: PathBuf,
    identity: FileIdentity,
}

impl DirectorySource {
    pub fn new(roots: impl IntoIterator<Item = PathBuf>) -> Self {
        let inner = DirInner {
            roots: Mutex::new(roots.into_iter().collect()),
            index: Mutex::new(None),
        };
        Self {
            inner: std::sync::Arc::new(inner),
        }
    }

    /// Add a file or directory; the index is rebuilt on the next lookup.
    pub fn add_root(&self, root: PathBuf) {
        let mut roots = self.inner.roots.lock().unwrap();
        if !roots.contains(&root) {
            roots.push(root);
        }
        *self.inner.index.lock().unwrap() = None;
    }
}

impl DirInner {
    fn build_index(roots: &[PathBuf]) -> HashMap<DebugId, Vec<Entry>> {
        let mut index: HashMap<DebugId, Vec<Entry>> = HashMap::new();
        for root in roots {
            for entry in walkdir::WalkDir::new(root)
                .follow_links(false)
                .into_iter()
                .filter_map(Result::ok)
            {
                if !entry.file_type().is_file()
                    || entry.metadata().map(|m| m.len() < 512).unwrap_or(true)
                {
                    continue;
                }
                for identity in identify(entry.path()) {
                    index.entry(identity.debug_id).or_default().push(Entry {
                        path: entry.path().to_path_buf(),
                        identity,
                    });
                }
            }
        }
        index
    }

    fn lookup(&self, id: DebugId) -> Option<Entry> {
        let mut guard = self.index.lock().unwrap();
        let roots = self.roots.lock().unwrap().clone();
        let index = guard.get_or_insert_with(|| Self::build_index(&roots));
        // A file with real debug info beats a stripped binary with the same id.
        index
            .get(&id)?
            .iter()
            .max_by_key(|e| (e.identity.has_debug_info, e.identity.has_symbols))
            .cloned()
    }
}

#[async_trait]
impl DebugFileSource for DirectorySource {
    fn name(&self) -> String {
        let roots = self.inner.roots.lock().unwrap();
        format!(
            "local files ({})",
            roots
                .iter()
                .map(|r| r.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }

    async fn find(
        &self,
        module: &ModuleRef,
        _build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>> {
        let Some(id) = module.debug_id else {
            return Ok(None);
        };
        // The first lookup walks the roots — keep that off the async executor.
        let inner = self.inner.clone();
        let hit = tokio::task::spawn_blocking(move || inner.lookup(id)).await?;
        Ok(hit.map(|e| FoundFile {
            origin: format!("local file {}", e.path.display()),
            has_debug_info: e.identity.has_debug_info,
            has_symbols: e.identity.has_symbols,
            path: e.path,
            in_dyld_cache: false,
        }))
    }
}
