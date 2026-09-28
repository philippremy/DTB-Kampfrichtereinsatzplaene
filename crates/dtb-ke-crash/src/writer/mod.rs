//! Out-of-process dump construction — the *heavy* half, compiled on every target except iOS (see the
//! crate's module doc comment).
//!
//! A platform's `capture` function reads the crashed process's memory (Mach ports on macOS, `ptrace`
//! on Linux, `OpenProcess` + `ClientPointers` on Windows) and returns minidump bytes; [`build_report`]
//! persists them, adds the `buildinfo`/`nsexception` streams, and digests the result into everything
//! `dtb-ke-ui`'s reporter dialog needs — pure, no spawn/exec of its own.

#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;
#[cfg(target_os = "linux")]
pub mod linux;

mod finish;
pub use finish::{Report, build_report};
