//! What a file *is*, in terms a crash dump can match: its debug id.
//!
//! A minidump names each module by `(code file, debug id)`. The debug id is the Mach-O `LC_UUID`, the ELF
//! build-id, or the PE's CodeView GUID + age — exactly what `samply_symbols::debug_id_for_object` computes
//! (it follows the Breakpad rules, so it agrees with what `minidump-writer` recorded). PDBs carry theirs
//! in the PDB info stream.

use std::path::Path;

pub use samply_symbols::debugid;
use samply_symbols::debugid::DebugId;
use samply_symbols::object::{self, Object, ObjectSection, ObjectSymbol, SymbolKind};
use samply_symbols::{debug_id_for_object, pdb};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FileType {
    MachO,
    Elf,
    Pe,
    Pdb,
}

/// One identifiable image inside a file (a fat Mach-O has several).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileIdentity {
    pub debug_id: DebugId,
    pub file_type: FileType,
    /// Carries DWARF / CodeView line info, not just a symbol table.
    pub has_debug_info: bool,
    /// Defines at least one named function symbol. A stripped binary has none: everything a symbolizer can say about
    /// it is a synthesized `fun_<address>` from the function-start addresses.
    pub has_symbols: bool,
}

impl FileIdentity {
    /// The id in the Breakpad spelling used on the wire and on disk (upper-case hex, no dashes, age appended).
    pub fn breakpad(&self) -> String {
        self.debug_id.breakpad().to_string()
    }
}

/// Total size of the DWARF sections (`.debug_*` / `.zdebug_*`) of a single (non-fat) object file; 0 if it has none
/// or is not an object file. Lets a caller check that a debug file split off a binary still holds all of the DWARF.
pub fn debug_section_bytes(path: &Path) -> u64 {
    let Ok(file) = std::fs::File::open(path) else {
        return 0;
    };
    // SAFETY: as in `identify` — a read-only map, every read bounds-checked by `object`.
    let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else {
        return 0;
    };
    let Ok(obj) = object::File::parse(&*map) else {
        return 0;
    };
    obj.sections()
        .filter(|s| {
            s.name()
                .is_ok_and(|n| n.starts_with(".debug_") || n.starts_with(".zdebug_"))
        })
        .map(|s| s.size())
        .sum()
}

const PDB_MAGIC: &[u8] = b"Microsoft C/C++ MSF 7.00\r\n\x1aDS\0\0\0";

/// The identities of the file at `path`; empty for anything that is not an object file or PDB.
pub fn identify(path: &Path) -> Vec<FileIdentity> {
    let Ok(file) = std::fs::File::open(path) else {
        return Vec::new();
    };
    // SAFETY: read-only map of a file we do not write; a concurrent truncation would at worst fault
    // the developer tool, and every read below is bounds-checked by `object`.
    let Ok(map) = (unsafe { memmap2::Mmap::map(&file) }) else {
        return Vec::new();
    };
    identify_bytes(&map, &file)
}

fn identify_bytes(data: &[u8], file: &std::fs::File) -> Vec<FileIdentity> {
    if data.starts_with(PDB_MAGIC) {
        return identify_pdb(file).into_iter().collect();
    }
    match object::FileKind::parse(data) {
        Ok(object::FileKind::MachOFat32) => object::read::macho::MachOFatFile32::parse(data)
            .map(|fat| fat_slices(fat.arches(), data))
            .unwrap_or_default(),
        Ok(object::FileKind::MachOFat64) => object::read::macho::MachOFatFile64::parse(data)
            .map(|fat| fat_slices(fat.arches(), data))
            .unwrap_or_default(),
        Ok(_) => identify_object(data).into_iter().collect(),
        Err(_) => Vec::new(),
    }
}

fn fat_slices<A: object::read::macho::FatArch>(arches: &[A], data: &[u8]) -> Vec<FileIdentity> {
    arches
        .iter()
        .filter_map(|a| a.data(data).ok())
        .filter_map(identify_object)
        .collect()
}

fn identify_object(data: &[u8]) -> Option<FileIdentity> {
    let obj = object::File::parse(data).ok()?;
    let file_type = match obj.format() {
        object::BinaryFormat::MachO => FileType::MachO,
        object::BinaryFormat::Elf => FileType::Elf,
        object::BinaryFormat::Pe => FileType::Pe,
        _ => return None,
    };
    Some(FileIdentity {
        debug_id: debug_id_for_object(&obj)?,
        file_type,
        // DWARF sections, or a Mach-O debug map (N_OSO): a local debug build keeps its DWARF in the object files and
        // points at them from the executable, which has no DWARF of its own — but the symbolizer follows the map.
        has_debug_info: obj.has_debug_symbols() || !obj.object_map().objects().is_empty(),
        has_symbols: has_function_symbols(&obj),
    })
}

/// A stripped binary still defines a handful of functions for the dynamic linker (`_main`, exports), so "any" says
/// nothing about whether its frames get names: a real symbol table has many.
const MIN_FUNCTION_SYMBOLS: usize = 8;

fn has_function_symbols(obj: &object::File) -> bool {
    obj.symbols()
        .chain(obj.dynamic_symbols())
        .filter(|s| s.kind() == SymbolKind::Text && s.is_definition())
        .take(MIN_FUNCTION_SYMBOLS)
        .count()
        >= MIN_FUNCTION_SYMBOLS
}

fn identify_pdb(file: &std::fs::File) -> Option<FileIdentity> {
    let mut pdb = pdb::PDB::open(file).ok()?;
    let info = pdb.pdb_information().ok()?;
    Some(FileIdentity {
        debug_id: DebugId::from_parts(info.guid, info.age),
        file_type: FileType::Pdb,
        has_debug_info: true,
        has_symbols: true,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unstripped_executable_has_symbols() {
        let ids = identify(&std::env::current_exe().unwrap());
        assert!(!ids.is_empty());
        assert!(
            ids.iter().all(|i| i.has_symbols),
            "a test executable defines named functions"
        );
    }

    #[test]
    fn a_local_debug_build_counts_as_having_debug_info() {
        // Mach-O keeps the DWARF in the object files (a debug map in the executable); ELF embeds it. Either way the
        // symbolizer can produce source lines, so the file must not be called "no debug info".
        if cfg!(any(target_os = "macos", target_os = "linux")) {
            let ids = identify(&std::env::current_exe().unwrap());
            assert!(ids.iter().all(|i| i.has_debug_info), "{ids:?}");
        }
    }
}
