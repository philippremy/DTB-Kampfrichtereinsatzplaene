//! Developer-only minidump debugger core (gpui-free, headless-testable).
//!
//! Pipeline: open a `.dtbkedmp` → read the build-info user stream (`dtb-ke-crash`'s `buildinfo`) → for every
//! module, find the **native** debug file (dSYM / PDB / DWARF / the executable itself) through a chain of
//! [`source::DebugFileSource`]s, matching on the debug id → symbolicate with `wholesym` and stackwalk with
//! `minidump-processor`. No Breakpad `.sym` files are involved.

pub mod build;
pub mod code;
pub mod discover;
pub mod dyld;
pub mod git;
pub mod highlight;
pub mod identity;
pub mod process;
pub mod rawdump;
pub mod remote;
pub mod resolve;
pub mod source;
pub mod sources;
pub mod symbolize;

pub use build::BuildInfo;
pub use discover::{Discovered, SystemFacts, discover};
pub use dyld::{DyldSharedCacheSource, add_caches_from};
pub use identity::{FileIdentity, FileType, ModuleRef};
pub use resolve::{ModuleResolution, Outcome, Resolution, Resolver};
pub use source::{DebugFileSource, DirectorySource, ExecutableSource, FoundFile};

/// The product name — window titles, the About window, the menu bar.
pub const NAME: &str = "DTB Kampfrichtereinsatzpläne Debugger";
/// Reverse-DNS identifier (ASCII; matches `dtb-ke-bundle`'s `meta::DEBUGGER`).
pub const IDENTIFIER: &str = "de.philippremy.DTB-Kampfrichtereinsatzplaene.Debugger";

/// Whether `path` is a crash report this tool opens: a `.dtbkedmp` (case-insensitive).
pub fn is_dump(path: &std::path::Path) -> bool {
    path.extension()
        .is_some_and(|e| e.eq_ignore_ascii_case(dtb_ke_crash::DUMP_EXTENSION))
}
