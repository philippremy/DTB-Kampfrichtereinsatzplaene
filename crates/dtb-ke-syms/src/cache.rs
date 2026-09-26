//! The dyld shared cache of a machine, read with `object`.
//!
//! `AppKit`, `CoreFoundation`, `libdispatch` … have no file on disk, only an image inside the cache. Every image is
//! **matched on its Mach-O UUID** (which changes with every OS build), so a dump from another build is reported
//! as missing rather than symbolicated with the wrong build's names.
//!
//! `wholesym` cannot open current caches — `samply-symbols` 0.24 guesses numeric subcache names (`.1`, `.01`) while
//! macOS 26+ names them `.25.dylddata`, `.79.dyldlinkedit` … — so this reads the real suffixes from the cache
//! header and lets `object` parse the rest, including the images' local symbols.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};

use object::macho::{DyldCacheHeader, DyldSubCacheEntryV2};
use object::read::macho::{DyldCache, DyldSubCacheSlice};
use object::{Endianness, Object, ObjectSegment, ObjectSymbol, SymbolKind};

pub type Uuid = [u8; 16];

/// One image's function symbols, sorted by address.
pub struct CacheSymbols {
    /// Address of the image's `__TEXT` segment in the cache — symbol addresses are relative to it.
    text_base: u64,
    /// `(cache address, demangled name)`.
    symbols: Vec<(u64, String)>,
}

/// A hit is only trusted this close below the address: past it, the "nearest preceding symbol" is really a
/// stripped stretch of code and naming it would be a guess.
pub const MAX_SYMBOL_REACH: u64 = 0x10_0000;

impl CacheSymbols {
    pub fn new(text_base: u64, mut symbols: Vec<(u64, String)>) -> Self {
        symbols.sort_by(|a, b| a.0.cmp(&b.0));
        symbols.dedup_by_key(|(a, _)| *a);
        Self { text_base, symbols }
    }

    /// The function containing `offset` bytes past the image's load address: `(name, function offset)`.
    pub fn lookup(&self, offset: u64) -> Option<(&str, u64)> {
        self.lookup_range(offset).map(|(name, start, _)| (name, start))
    }

    /// Like [`lookup`](Self::lookup), plus where the function ends (the next symbol, capped at the reach limit):
    /// `(name, start, end)`, both relative to the image.
    pub fn lookup_range(&self, offset: u64) -> Option<(&str, u64, u64)> {
        let address = self.text_base + offset;
        let after = self.symbols.partition_point(|(a, _)| *a <= address);
        let (start, name) = self.symbols.get(after.checked_sub(1)?)?;
        if address - start > MAX_SYMBOL_REACH {
            return None;
        }
        let end = self.symbols.get(after).map_or(start + MAX_SYMBOL_REACH, |(next, _)| (*next).min(start + MAX_SYMBOL_REACH));
        Some((name.as_str(), start - self.text_base, end - self.text_base))
    }

    pub fn len(&self) -> usize {
        self.symbols.len()
    }

    pub fn is_empty(&self) -> bool {
        self.symbols.is_empty()
    }

    /// Test / tooling access: `(cache address, name)`.
    pub fn raw(&self) -> (u64, &[(u64, String)]) {
        (self.text_base, &self.symbols)
    }
}

/// The mapped cache of one machine / runtime.
pub struct DyldCacheReader {
    base: PathBuf,
    cache: DyldCache<'static, Endianness, &'static [u8]>,
    /// Install name → position in the image table, so a lookup is not a scan of ~4000 images.
    index: HashMap<&'static str, usize>,
    /// Built on first use per image (a framework's symbol table is large — AppKit: ~190k — and a dump loads
    /// over a thousand images, of which only a handful appear in a stack).
    memo: Mutex<HashMap<String, Option<Arc<CacheSymbols>>>>,
}

fn hex(uuid: &Uuid) -> String {
    uuid.iter().map(|b| format!("{b:02x}")).collect()
}

impl DyldCacheReader {
    /// The cache of this Mac, opened once. `None` off macOS, or when no readable cache exists.
    fn host() -> Option<&'static DyldCacheReader> {
        static HOST: OnceLock<Option<DyldCacheReader>> = OnceLock::new();
        HOST.get_or_init(|| match Self::open_host() {
            Ok(reader) => Some(reader),
            Err(err) => {
                log::debug!("no usable dyld shared cache: {err}");
                None
            }
        })
        .as_ref()
    }

    fn open_host() -> Result<Self, String> {
        if !cfg!(target_os = "macos") {
            return Err("not macOS".into());
        }
        let archs: &[&str] = if cfg!(target_arch = "aarch64") { &["arm64e"] } else { &["x86_64h", "x86_64"] };
        for dir in ["/System/Volumes/Preboot/Cryptexes/OS/System/Library/dyld", "/System/Library/dyld"] {
            for arch in archs {
                let base = PathBuf::from(format!("{dir}/dyld_shared_cache_{arch}"));
                if base.is_file() {
                    return Self::open(&base);
                }
            }
        }
        Err("no dyld_shared_cache_* file found".into())
    }

    /// The mapped files are leaked on purpose: the cache borrows them for the life of the process, and each
    /// cache is opened at most once.
    pub fn open(base: &Path) -> Result<Self, String> {
        fn map(path: &Path) -> Result<&'static [u8], String> {
            let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            // SAFETY: read-only map of a system file that is never written while the process runs.
            let mmap = unsafe { memmap2::Mmap::map(&file) }.map_err(|e| format!("{}: {e}", path.display()))?;
            let leaked: &'static memmap2::Mmap = Box::leak(Box::new(mmap));
            Ok(&leaked[..])
        }
        let err = |e: object::Error| e.to_string();

        let main = map(base)?;
        let header = DyldCacheHeader::<Endianness>::parse(main).map_err(err)?;
        let (_, endian) = header.parse_magic().map_err(err)?;
        let name = base.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let with_suffix = |suffix: &str| base.with_file_name(format!("{name}{suffix}"));

        let mut subs: Vec<&'static [u8]> = Vec::new();
        match header.subcaches(endian, main).map_err(err)? {
            Some(DyldSubCacheSlice::V2(entries)) => {
                for entry in entries {
                    let entry: &DyldSubCacheEntryV2<Endianness> = entry;
                    let end = entry.file_suffix.iter().position(|&b| b == 0).unwrap_or(entry.file_suffix.len());
                    subs.push(map(&with_suffix(&String::from_utf8_lossy(&entry.file_suffix[..end])))?);
                }
            }
            // macOS 12: numbered `.1`, `.2` …
            Some(DyldSubCacheSlice::V1(entries)) => {
                for i in 1..=entries.len() {
                    subs.push(map(&with_suffix(&format!(".{i}")))?);
                }
            }
            _ => {}
        }
        if header.symbols_subcache_uuid(endian).is_some() {
            subs.push(map(&with_suffix(".symbols"))?);
        }
        let cache = DyldCache::<Endianness>::parse(main, &subs).map_err(err)?;
        log::info!("opened the dyld shared cache {} ({} images)", base.display(), cache.images().count());
        let index = cache.images().enumerate().filter_map(|(i, img)| Some((img.path().ok()?, i))).collect();
        Ok(Self { base: base.to_path_buf(), cache, index, memo: Mutex::new(HashMap::new()) })
    }

    pub fn base(&self) -> &Path {
        &self.base
    }

    fn uuid_of(&self, path: &str) -> Option<Uuid> {
        let image = self.cache.images().nth(*self.index.get(path)?)?;
        image.parse_object().ok()?.mach_uuid().ok().flatten()
    }

    /// Is the image at `path` in this cache **and** the exact build (`uuid`) the crash ran? Cheap: no symbols
    /// are read.
    pub fn contains(&self, path: &str, uuid: &Uuid) -> bool {
        self.uuid_of(path).as_ref() == Some(uuid)
    }

    /// The function symbols of the image at `path`, **only if** its UUID is `uuid`.
    pub fn symbols(&self, path: &str, uuid: &Uuid) -> Option<Arc<CacheSymbols>> {
        let key = format!("{path}#{}", hex(uuid));
        if let Some(hit) = self.memo.lock().unwrap().get(&key) {
            return hit.clone();
        }
        let built = self.build(path, uuid).map(Arc::new);
        self.memo.lock().unwrap().insert(key, built.clone());
        built
    }

    fn build(&self, path: &str, uuid: &Uuid) -> Option<CacheSymbols> {
        if !self.contains(path, uuid) {
            log::debug!("{path}: not in the cache, or a different build than the crash's ({})", hex(uuid));
            return None;
        }
        let object = self.cache.images().nth(*self.index.get(path)?)?.parse_object().ok()?;
        let text_base = object.segments().find(|s| s.name().ok().flatten() == Some("__TEXT"))?.address();
        let symbols: Vec<(u64, String)> = object
            .symbols()
            .filter(|s| s.kind() == SymbolKind::Text && s.address() != 0)
            .filter_map(|s| Some((s.address(), display_name(s.name().ok()?))))
            .filter(|(_, name)| !name.is_empty())
            .collect();
        Some(CacheSymbols::new(text_base, symbols))
    }
}

/// `_pthread_start` → `pthread_start` (the C ABI's leading underscore), C++ / Rust names demangled.
pub fn display_name(raw: &str) -> String {
    let name = raw.strip_prefix('_').unwrap_or(raw);
    if let Ok(d) = rustc_demangle::try_demangle(name) {
        return format!("{d:#}");
    }
    if name.starts_with("_Z") {
        if let Ok(sym) = cpp_demangle::Symbol::new(name) {
            if let Ok(s) = sym.demangle(&cpp_demangle::DemangleOptions::default()) {
                return s;
            }
        }
    }
    name.to_owned()
}

// ── all known caches: this machine's, plus any registered (a simulator runtime's, one from an IPSW) ──

static EXTRA: Mutex<Vec<&'static DyldCacheReader>> = Mutex::new(Vec::new());

fn readers() -> Vec<&'static DyldCacheReader> {
    let mut all: Vec<&'static DyldCacheReader> = DyldCacheReader::host().into_iter().collect();
    all.extend(EXTRA.lock().unwrap().iter().copied());
    all
}

/// Whether this machine has a readable dyld cache of its own.
pub fn host_available() -> bool {
    DyldCacheReader::host().is_some()
}

/// Register the cache whose main file is `base`. `Ok(false)` if it was already known.
pub fn register(base: &Path) -> Result<bool, String> {
    if readers().iter().any(|r| r.base == base) {
        return Ok(false);
    }
    let reader = DyldCacheReader::open(base)?;
    let leaked: &'static DyldCacheReader = Box::leak(Box::new(reader));
    EXTRA.lock().unwrap().push(leaked);
    Ok(true)
}

/// Is the image `install_name` in any known cache **and** the exact build (`uuid`) the crash ran?
pub fn contains(install_name: &str, uuid: &Uuid) -> bool {
    readers().iter().any(|r| r.contains(install_name, uuid))
}

/// The symbols of that image from whichever known cache holds it.
pub fn symbols(install_name: &str, uuid: &Uuid) -> Option<Arc<CacheSymbols>> {
    readers().iter().find_map(|r| r.symbols(install_name, uuid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_names_the_containing_function_and_refuses_far_guesses() {
        let s = CacheSymbols::new(0x1000, vec![(0x1000, "a".into()), (0x1100, "b".into())]);
        assert_eq!(s.lookup(0x0), Some(("a", 0x0)));
        assert_eq!(s.lookup(0xff), Some(("a", 0x0)));
        assert_eq!(s.lookup(0x100), Some(("b", 0x100)));
        assert_eq!(s.lookup(0x10f), Some(("b", 0x100)));
        assert_eq!(CacheSymbols::new(0x1000, vec![(0x2000, "x".into())]).lookup(0), None);
        assert_eq!(s.lookup(0x100 + MAX_SYMBOL_REACH), Some(("b", 0x100)));
        assert_eq!(s.lookup(0x100 + MAX_SYMBOL_REACH + 1), None);
        assert_eq!(CacheSymbols::new(0x1000, vec![]).lookup(0), None);
    }

    #[test]
    fn ranges_end_at_the_next_symbol() {
        let s = CacheSymbols::new(0x1000, vec![(0x1000, "a".into()), (0x1100, "b".into())]);
        assert_eq!(s.lookup_range(0x10), Some(("a", 0x0, 0x100)));
        assert_eq!(s.lookup_range(0x100), Some(("b", 0x100, 0x100 + MAX_SYMBOL_REACH)));
    }

    #[test]
    fn names_lose_the_c_underscore_and_get_demangled() {
        assert_eq!(display_name("_pthread_start"), "pthread_start");
        assert_eq!(display_name("+[NSObject load]"), "+[NSObject load]");
        assert_eq!(display_name("__ZN3foo3barEv"), "foo::bar()");
    }

    /// Against this Mac's real cache; skipped where there is none (other OS, CI).
    #[test]
    fn this_macs_cache_symbolicates_libdispatch_and_rejects_a_wrong_uuid() {
        let Some(cache) = DyldCacheReader::host() else { return };
        let path = "/usr/lib/system/libdispatch.dylib";
        // The cache is there, so a miss here is a real failure (a parser that rejects it), not a skip.
        let uuid = cache.uuid_of(path).expect("libdispatch's UUID from the cache");
        let wrong = [0u8; 16];

        assert!(cache.contains(path, &uuid));
        assert!(!cache.contains(path, &wrong));
        assert!(cache.symbols(path, &wrong).is_none());
        assert!(!cache.contains("/usr/lib/does-not-exist.dylib", &uuid));

        let symbols = cache.symbols(path, &uuid).expect("libdispatch symbols");
        assert!(symbols.len() > 500, "{} symbols", symbols.len());
        let (base, all) = symbols.raw();
        let (address, name) = &all[all.len() / 2];
        let (found, offset) = symbols.lookup(address - base).expect("hit");
        assert_eq!((found, offset), (name.as_str(), address - base));
    }
}
