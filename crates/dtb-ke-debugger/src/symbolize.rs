//! A `minidump-unwind` [`SymbolProvider`] over native debug files, keyed by module base address.
//! Symbolication only: unwinding falls back to the processor's frame-pointer / scan walkers (CFI from
//! the executable's unwind info is a follow-up — it needs the code file, not just the debug file).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use dtb_ke_crash::syshints::{Hints, Quality};
use minidump::Module;
use minidump_unwind::{
    FileError, FileKind, FillSymbolError, FrameSymbolizer, FrameWalker, SymbolProvider,
};
use tokio::sync::Mutex;
use wholesym::{
    LookupAddress, MultiArchDisambiguator, SymbolManager, SymbolManagerConfig, SymbolMap,
};

use crate::resolve::Resolution;

pub struct NativeSymbolProvider {
    maps: HashMap<u64, Mutex<SymbolMap>>,
    /// Modules that live in this Mac's dyld shared cache, by module base → `(install name, debug id)`. Their
    /// symbols are read from the cache when a frame first needs them, not up front.
    cached: HashMap<u64, (String, samply_symbols::debugid::DebugId)>,
    files: HashMap<u64, PathBuf>,
    /// Names the crashed machine computed for its system frames (the dump's hints stream) — the fallback for
    /// system modules no source could locate. Keyed by module base → image UUID.
    hints: Option<Arc<Hints>>,
    uuids: HashMap<u64, [u8; 16]>,
}

impl NativeSymbolProvider {
    /// Load a symbol map for every resolved module. A file wholesym cannot read is logged and skipped —
    /// that module simply stays unsymbolicated.
    pub async fn load(resolution: &Resolution, hints: Option<Arc<Hints>>) -> Self {
        let manager = SymbolManager::with_config(SymbolManagerConfig::new());
        let mut maps = HashMap::new();
        let mut files = HashMap::new();
        let mut cached = HashMap::new();
        for (module, found) in resolution.found() {
            if found.in_dyld_cache {
                if let Some(id) = module.debug_id {
                    cached.insert(module.base, (module.install_name().to_owned(), id));
                }
                continue;
            }
            // A universal Mach-O holds one image per architecture; the module's debug id picks ours.
            let which = module.debug_id.map(MultiArchDisambiguator::DebugId);
            match manager
                .load_symbol_map_for_binary_at_path(&found.path, which)
                .await
            {
                Ok(map) => {
                    maps.insert(module.base, Mutex::new(map));
                    files.insert(module.base, found.path.clone());
                }
                Err(err) => log::warn!("cannot load symbols from {}: {err}", found.path.display()),
            }
        }
        let uuids = resolution
            .modules
            .iter()
            .filter_map(|m| Some((m.module.base, *m.module.debug_id?.uuid().as_bytes())))
            .collect();
        Self {
            maps,
            cached,
            files,
            hints,
            uuids,
        }
    }

    pub fn symbolicated_modules(&self) -> usize {
        self.maps.len() + self.cached.len()
    }
}

impl NativeSymbolProvider {
    /// The dump's own system-symbol hints for a module nothing else resolved. Approximate ones (iOS `dladdr`:
    /// exported symbols only, the nearest one *before* the address) are marked with `≈`.
    fn fill_from_hints(
        &self,
        module: &(dyn Module + Sync),
        frame: &mut (dyn FrameSymbolizer + Send),
    ) -> Result<(), FillSymbolError> {
        let (Some(hints), Some(uuid)) = (&self.hints, self.uuids.get(&module.base_address()))
        else {
            return Err(FillSymbolError {});
        };
        let offset = frame.get_instruction().wrapping_sub(module.base_address());
        let Some(entry) = hints.lookup(uuid, offset) else {
            return Err(FillSymbolError {});
        };
        let name = match hints.quality {
            Quality::Exact => entry.name.clone(),
            Quality::Approximate => format!("≈ {}", entry.name),
        };
        frame.set_function(&name, module.base_address() + entry.start, 0);
        Ok(())
    }
}

#[async_trait]
impl SymbolProvider for NativeSymbolProvider {
    async fn fill_symbol(
        &self,
        module: &(dyn Module + Sync),
        frame: &mut (dyn FrameSymbolizer + Send),
    ) -> Result<(), FillSymbolError> {
        if let Some((path, id)) = self.cached.get(&module.base_address()) {
            let Some(symbols) = crate::dyld::symbols(path, *id) else {
                return Ok(());
            };
            let offset = frame.get_instruction().wrapping_sub(module.base_address());
            if let Some((name, function_offset)) = symbols.lookup(offset) {
                frame.set_function(name, module.base_address() + function_offset, 0);
            }
            return Ok(());
        }
        if !self.maps.contains_key(&module.base_address()) {
            return self.fill_from_hints(module, frame);
        }
        let map = self
            .maps
            .get(&module.base_address())
            .ok_or(FillSymbolError {})?;
        let Ok(relative) =
            u32::try_from(frame.get_instruction().wrapping_sub(module.base_address()))
        else {
            return Ok(());
        };
        let Some(info) = map
            .lock()
            .await
            .lookup(LookupAddress::Relative(relative))
            .await
        else {
            return Ok(());
        };

        let function_base = module.base_address() + u64::from(info.symbol.address);
        frame.set_function(&info.symbol.name, function_base, 0);

        // `frames` runs innermost-first; the outermost (real) frame carries the function's own file/line,
        // everything before it is inlined into it.
        if let Some(frames) = info.frames {
            let mut iter = frames.into_iter().rev();
            if let Some(outer) = iter.next() {
                if let Some(path) = outer.file_path {
                    frame.set_source_file(
                        path.raw_path(),
                        outer.line_number.unwrap_or(0),
                        function_base,
                    );
                }
            }
            for inline in iter {
                frame.add_inline_frame(
                    inline.function.as_deref().unwrap_or(""),
                    inline.file_path.as_ref().map(|p| p.raw_path()),
                    inline.line_number,
                );
            }
        }
        Ok(())
    }

    async fn walk_frame(
        &self,
        _module: &(dyn Module + Sync),
        _walker: &mut (dyn FrameWalker + Send),
    ) -> Option<()> {
        None
    }

    async fn get_file_path(
        &self,
        module: &(dyn Module + Sync),
        _kind: FileKind,
    ) -> Result<PathBuf, FileError> {
        self.files
            .get(&module.base_address())
            .cloned()
            .ok_or(FileError::NotFound)
    }
}
