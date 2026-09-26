//! What a file *is*, in terms a crash dump can match: its debug id.
//!
//! A minidump names each module by `(code file, debug id)`. The debug id is the Mach-O `LC_UUID`, the ELF
//! build-id, or the PE's CodeView GUID + age — exactly what `samply_symbols::debug_id_for_object` computes
//! (it follows the Breakpad rules, so it agrees with what `minidump-writer` recorded). PDBs carry theirs
//! in the PDB info stream.

use std::path::{Path, PathBuf};

use minidump::{Minidump, MinidumpModuleList, Module};
use samply_symbols::debugid::DebugId;
use samply_symbols::object::{self, Object};
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
        has_debug_info: obj.has_debug_symbols(),
    })
}

fn identify_pdb(file: &std::fs::File) -> Option<FileIdentity> {
    let mut pdb = pdb::PDB::open(file).ok()?;
    let info = pdb.pdb_information().ok()?;
    Some(FileIdentity {
        debug_id: DebugId::from_parts(info.guid, info.age),
        file_type: FileType::Pdb,
        has_debug_info: true,
    })
}

/// A module as the dump recorded it — the query every debug-file source answers.
#[derive(Clone, Debug)]
pub struct ModuleRef {
    pub base: u64,
    pub size: u64,
    /// The module's code file path on the crashed machine.
    pub code_file: String,
    /// The debug file's name (`X.pdb`, or the binary's name for Mach-O / ELF).
    pub debug_file: Option<String>,
    pub debug_id: Option<DebugId>,
}

impl ModuleRef {
    pub fn from_module(m: &impl Module) -> Self {
        Self {
            base: m.base_address(),
            size: m.size(),
            code_file: m.code_file().into_owned(),
            debug_file: m.debug_file().map(|d| d.into_owned()),
            debug_id: m.debug_identifier(),
        }
    }

    /// The file name part of the code file, for display and directory-layout lookups.
    pub fn short_name(&self) -> &str {
        self.code_file
            .rsplit(['/', '\\'])
            .next()
            .filter(|s| !s.is_empty())
            .unwrap_or(&self.code_file)
    }

    /// Part of the OS (dyld cache, `/usr/lib`, `C:\\Windows`…): never in a symbol archive, so asking a
    /// server about it is wasted round-trips.
    pub fn is_system(&self) -> bool {
        const PREFIXES: &[&str] = &[
            "/usr/lib/",
            "/System/",
            "/lib/",
            "/lib64/",
            "/usr/lib64/",
            "/usr/libexec/",
            "/private/preboot/",
            "[vdso]",
        ];
        let path = self.install_name();
        PREFIXES.iter().any(|p| path.starts_with(p))
            || path.to_ascii_lowercase().contains(":\\windows\\")
    }

    /// The path the OS itself knows the image by. A simulator process reports
    /// `<…>/<runtime>.simruntime/Contents/Resources/RuntimeRoot/usr/lib/libdispatch.dylib`, while the runtime's
    /// dyld cache lists `/usr/lib/libdispatch.dylib`.
    pub fn install_name(&self) -> &str {
        match self.code_file.find("/RuntimeRoot/") {
            Some(i) => &self.code_file[i + "/RuntimeRoot".len()..],
            None => &self.code_file,
        }
    }

    pub fn code_path(&self) -> PathBuf {
        PathBuf::from(&self.code_file)
    }
}

/// Every module of the dump, the main executable first (as the dump lists it).
pub fn modules_of<'a, T: std::ops::Deref<Target = [u8]> + 'a>(
    dump: &Minidump<'a, T>,
) -> Vec<ModuleRef> {
    dump.get_stream::<MinidumpModuleList>()
        .map(|list| list.iter().map(ModuleRef::from_module).collect())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn module(path: &str) -> ModuleRef {
        ModuleRef {
            base: 0,
            size: 0,
            code_file: path.into(),
            debug_file: None,
            debug_id: None,
        }
    }

    #[test]
    fn simulator_paths_are_normalised_to_the_install_name() {
        let sim = module(
            "/Library/Developer/CoreSimulator/Volumes/iOS_23F77/Library/Developer/CoreSimulator/Profiles/Runtimes/iOS 26.5.simruntime/Contents/Resources/RuntimeRoot/usr/lib/system/libdispatch.dylib",
        );
        assert_eq!(sim.install_name(), "/usr/lib/system/libdispatch.dylib");
        assert!(sim.is_system());

        let app = module(
            "/Users/x/Library/Developer/CoreSimulator/Devices/ABC/data/Containers/Bundle/Application/D/App.app/App",
        );
        assert_eq!(app.install_name(), app.code_file);
        assert!(!app.is_system());
        assert!(
            module("/System/Library/Frameworks/AppKit.framework/Versions/C/AppKit").is_system()
        );
    }
}
