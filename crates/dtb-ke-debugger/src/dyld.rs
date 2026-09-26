//! System frameworks on macOS: images that exist only inside a **dyld shared cache**.
//!
//! The reader (`dtb_ke_syms::cache`) is shared with the crash reporter, which uses it on the crashed machine to
//! write the system-symbol hints into the dump; here it symbolicates from this Mac's cache, or from any cache
//! the developer supplies (a simulator runtime's, one from an IPSW). Every image is matched on its Mach-O UUID,
//! so a dump from another OS build is reported as missing instead of being given the wrong build's names.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use dtb_ke_syms::cache::{self, CacheSymbols};
use samply_symbols::debugid::DebugId;

use crate::build::BuildInfo;
use crate::identity::ModuleRef;
use crate::source::{DebugFileSource, FoundFile};

fn uuid_of(id: DebugId) -> [u8; 16] {
    *id.uuid().as_bytes()
}

/// The file name of a cache's *main* file (`dyld_shared_cache_arm64e`, `dyld_sim_shared_cache_x86_64`); its
/// subcaches carry a `.NN…` suffix and are found through the main file's header.
fn is_main_cache_name(name: &str) -> bool {
    name.starts_with("dyld_") && name.contains("shared_cache") && !name.contains('.')
}

fn has_cache_magic(path: &Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 7];
    std::fs::File::open(path)
        .and_then(|mut f| f.read_exact(&mut magic))
        .is_ok()
        && &magic == b"dyld_v1"
}

/// Register the dyld caches found at `paths` (a cache's main file, or a directory searched a few levels deep).
/// Returns how many *new* caches were opened; unreadable candidates are logged and skipped.
pub fn add_caches_from(paths: &[PathBuf]) -> usize {
    let mut candidates: Vec<PathBuf> = Vec::new();
    for path in paths {
        if path.is_dir() {
            candidates.extend(
                walkdir::WalkDir::new(path)
                    .max_depth(6)
                    .follow_links(false)
                    .into_iter()
                    .filter_map(Result::ok)
                    .filter(|e| e.file_type().is_file())
                    .map(|e| e.into_path()),
            );
        } else {
            candidates.push(path.clone());
        }
    }

    let mut opened = 0;
    for path in candidates {
        let named = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_main_cache_name);
        if !named || !has_cache_magic(&path) {
            continue;
        }
        match cache::register(&path) {
            Ok(true) => opened += 1,
            Ok(false) => {}
            Err(err) => log::warn!("cannot read the dyld cache {}: {err}", path.display()),
        }
    }
    opened
}

/// Is the image `install_name` in any known cache **and** the exact build (`id`) the dump ran?
pub fn contains(install_name: &str, id: DebugId) -> bool {
    cache::contains(install_name, &uuid_of(id))
}

/// The symbols of that image from whichever known cache holds it.
pub fn symbols(install_name: &str, id: DebugId) -> Option<Arc<CacheSymbols>> {
    cache::symbols(install_name, &uuid_of(id))
}

/// Finds system images in the known dyld caches; the result is symbolicated by
/// [`NativeSymbolProvider`](crate::symbolize::NativeSymbolProvider) through this module's [`symbols`].
pub struct DyldSharedCacheSource;

#[async_trait]
impl DebugFileSource for DyldSharedCacheSource {
    fn name(&self) -> String {
        "dyld shared caches".into()
    }

    async fn find(
        &self,
        module: &ModuleRef,
        _build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>> {
        let Some(id) = module.debug_id else {
            return Ok(None);
        };
        // Only OS-provided images live in a cache. A module with a real file is the other sources' business.
        if !module.is_system() || module.code_path().is_file() {
            return Ok(None);
        }
        let (path, install_name) = (module.code_path(), module.install_name().to_owned());
        let found = tokio::task::spawn_blocking(move || contains(&install_name, id)).await?;
        Ok(found.then(|| FoundFile {
            path,
            has_debug_info: false,
            origin: "dyld shared cache".into(),
            in_dyld_cache: true,
        }))
    }
}
