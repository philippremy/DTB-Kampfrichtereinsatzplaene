//! The build-info **user stream**: what the crashed build was, so a debugger can find its debug files.
//!
//! The payload is plain UTF-8, one `key=value` per line (`\n`-separated, values single-line). It is
//! deliberately not a binary schema: it survives adding keys, and `strings` shows it. The app builds
//! the text once (`dtb-ke-ui`'s `build_info::stream_text`) and hands it to [`crate::library::Config`];
//! the crash helper appends it to the finished dump as a stream of type [`STREAM_TYPE`] (see
//! [`crate::patch`]); the iOS assembler writes it natively.
//!
//! Stream type: in the user-defined range (> `0xffff`), clear of Breakpad (`0x4767…`), Crashpad
//! (`0x4350…`) and Mozilla (`0x4d7a…`). It is the ASCII bytes `DTKE`.

use std::collections::BTreeMap;

/// The minidump stream type carrying the build info.
pub const STREAM_TYPE: u32 = 0x4454_4b45;

/// Well-known keys (the app may add more; readers must ignore what they don't know).
pub mod key {
    pub const APP_VERSION: &str = "app_version";
    pub const COMMIT: &str = "commit";
    pub const TARGET: &str = "target";
    pub const PROFILE: &str = "profile";
    pub const WORKSPACE_ROOT: &str = "workspace_root";
}

/// Parse the stream payload into its key/value pairs. Lines without `=` are ignored; the first `=`
/// splits, so values may themselves contain `=`.
pub fn parse(text: &str) -> BTreeMap<&str, &str> {
    text.lines()
        .filter_map(|l| l.split_once('='))
        .filter(|(k, _)| !k.is_empty())
        .collect()
}

/// Render pairs into the payload text. Newlines / NULs inside a value are flattened to spaces so the
/// one-line-per-pair invariant (and the NUL-terminated crash-time pipe field) always holds.
pub fn render<'a>(pairs: impl IntoIterator<Item = (&'a str, String)>) -> String {
    let mut out = String::new();
    for (k, v) in pairs {
        out.push_str(k);
        out.push('=');
        out.extend(v.chars().map(|c| if matches!(c, '\n' | '\r' | '\0') { ' ' } else { c }));
        out.push('\n');
    }
    out
}

mod global {
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    static PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
    static LEN: AtomicUsize = AtomicUsize::new(0);
    static EMPTY: [u8; 1] = [0];

    /// Copy `text` (NUL-terminated, embedded NULs dropped) into a leaked buffer once, at install. The
    /// crash path then only does atomic loads and a `write` — no allocation.
    pub(crate) fn set(text: &str) {
        let mut bytes: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
        bytes.push(0);
        let len = bytes.len() - 1;
        let leaked: &'static mut [u8] = Box::leak(bytes.into_boxed_slice());
        LEN.store(len, Ordering::SeqCst);
        PTR.store(leaked.as_mut_ptr(), Ordering::SeqCst);
    }

    /// NUL-terminated payload for the crash-time pipe field (an empty field if never set).
    #[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
    pub(crate) fn cstr_ptr() -> *const u8 {
        let p = PTR.load(Ordering::SeqCst);
        if p.is_null() { EMPTY.as_ptr() } else { p }
    }

    /// The payload text (empty if never set). For non-crash-time consumers (the iOS session record).
    #[cfg(target_os = "ios")]
    pub(crate) fn text() -> &'static str {
        let p = PTR.load(Ordering::SeqCst);
        if p.is_null() {
            return "";
        }
        // SAFETY: `set` leaked a valid UTF-8 buffer of exactly `LEN` bytes and never frees it.
        unsafe { core::str::from_utf8_unchecked(core::slice::from_raw_parts(p, LEN.load(Ordering::SeqCst))) }
    }
}

pub(crate) use global::*;
