//! System-library symbolication on the machine that crashed.
//!
//! Two engines produce [`dtb_ke_crash::syshints`] entries as cancellable background [`Job`]s:
//!
//! * [`spawn_dyld_cache`] — macOS: reads the machine's own dyld shared cache with `object`, including the local
//!   symbols (ObjC methods, private functions). Exact; the debugger uses the same reader.
//! * [`spawn_dladdr`] — iOS: `dladdr` on the images loaded in this process. Exported symbols only, so it is
//!   marked *approximate*.
//!
//! The reader itself ([`cache`]) is also what `dtb-ke-debugger` symbolicates system frames with.

pub mod cache;
mod engine;

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub mod approx;

pub use engine::{Job, spawn_dyld_cache};

#[cfg(any(target_os = "macos", target_os = "ios"))]
pub use engine::spawn_dladdr;

/// `/…/RuntimeRoot/usr/lib/x.dylib` (how a simulator process names a library) → `/usr/lib/x.dylib` (how the
/// runtime's dyld cache names it). Other paths are returned unchanged.
pub fn install_name(path: &str) -> &str {
    match path.find("/RuntimeRoot/") {
        Some(i) => &path[i + "/RuntimeRoot".len()..],
        None => path,
    }
}

/// Part of the OS — the only images worth asking a system cache about.
pub fn is_system_path(path: &str) -> bool {
    const PREFIXES: &[&str] = &["/usr/lib/", "/System/", "/usr/libexec/", "/private/preboot/", "/lib/", "/lib64/", "/usr/lib64/"];
    let p = install_name(path);
    PREFIXES.iter().any(|x| p.starts_with(x))
}
