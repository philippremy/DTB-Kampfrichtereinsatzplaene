//! What a file *is*, in terms a crash dump can match: its debug id. The file-reading half lives in
//! `dtb-ke-symid` (shared with the symbol server); this module adds the dump-side [`ModuleRef`].

use std::path::PathBuf;

use minidump::{Minidump, MinidumpModuleList, Module};
use samply_symbols::debugid::DebugId;

pub use dtb_ke_symid::{FileIdentity, FileType, identify};

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
