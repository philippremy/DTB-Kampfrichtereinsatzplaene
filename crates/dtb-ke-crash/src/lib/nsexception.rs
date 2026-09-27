//! The uncaught-`NSException` **user stream** — macOS only: what an uncaught Objective-C exception
//! said right before AppKit aborted the process.
//!
//! There is no standardized minidump stream for this (Breakpad/Crashpad only translate the *Mach*
//! exception that follows — which our own `EXC_PORT` handler already captures as the dump's usual
//! `Exception` stream — into their format; `NSException` is a pure Foundation construct with no
//! representation there). So this is ours, the same way [`crate::buildinfo`] and [`crate::syshints`]
//! are: the app builds the payload once, in `on_uncaught_exception` (`macos::install`) — which still
//! runs in ordinary, fully-valid context, well before the actual fault — and hands it to the crash
//! helper as one more pipe field; the helper appends it to the finished dump as a stream of type
//! [`STREAM_TYPE`] (see [`crate::patch`]).
//!
//! The payload is plain UTF-8: `name=…` / `reason=…` (single-line, values flattened like
//! `buildinfo`'s), then `frame_count=N` followed by exactly `N` lines — one raw
//! `-[NSException callStackSymbols]` entry each. Those lines are pre-formatted by Foundation (module
//! + best-effort symbol) at the *throw* site, which is usually a different — and more useful —
//! location than the dump's own thread list, itself captured where the abort trap eventually landed
//! (inside AppKit's own exception-catching machinery). A reader can't re-symbolicate them further;
//! show them as plain text, not `Frame` rows.
//!
//! Stream type: the ASCII bytes `DTKX`, next in the `DTKE` (build-info) / `DTKF` (system-symbol
//! hints) sequence — in the user-defined range (> `0xffff`), clear of Breakpad (`0x4767…`), Crashpad
//! (`0x4350…`) and Mozilla (`0x4d7a…`).

/// The minidump stream type carrying the uncaught-`NSException` info.
pub const STREAM_TYPE: u32 = 0x4454_4b58;

/// What an uncaught `NSException` said.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct NsException {
    pub name: String,
    pub reason: String,
    /// Raw `-[NSException callStackSymbols]` lines, throw-site order (frame 0 first).
    pub frames: Vec<String>,
}

/// Newlines/NULs flattened to spaces, so the one-line-per-field invariant always holds.
fn flatten(s: &str) -> String {
    s.chars().map(|c| if matches!(c, '\n' | '\r' | '\0') { ' ' } else { c }).collect()
}

/// Render into the stream payload text.
pub fn render(info: &NsException) -> String {
    let mut out = String::new();
    out.push_str("name=");
    out.push_str(&flatten(&info.name));
    out.push('\n');
    out.push_str("reason=");
    out.push_str(&flatten(&info.reason));
    out.push('\n');
    out.push_str("frame_count=");
    out.push_str(&info.frames.len().to_string());
    out.push('\n');
    for frame in &info.frames {
        out.push_str(&flatten(frame));
        out.push('\n');
    }
    out
}

/// Parse the stream payload back. `None` for anything that doesn't match the shape `render` writes
/// (a foreign or truncated stream).
pub fn parse(text: &str) -> Option<NsException> {
    let mut lines = text.lines();
    let name = lines.next()?.strip_prefix("name=")?.to_owned();
    let reason = lines.next()?.strip_prefix("reason=")?.to_owned();
    let count: usize = lines.next()?.strip_prefix("frame_count=")?.parse().ok()?;
    let frames: Vec<String> = lines.by_ref().take(count).map(str::to_owned).collect();
    Some(NsException { name, reason, frames })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> NsException {
        NsException {
            name: "NSInternalInconsistencyException".into(),
            reason: "An instance 0x1 of class NSWindow was deallocated while key value observers \
                     were still registered with it."
                .into(),
            frames: vec![
                "0   CoreFoundation    0x00007ff8 __exceptionPreprocess + 176".into(),
                "1   libobjc.A.dylib   0x00007ff8 objc_exception_throw + 48".into(),
            ],
        }
    }

    #[test]
    fn round_trips() {
        let ex = sample();
        assert_eq!(parse(&render(&ex)), Some(ex));
    }

    #[test]
    fn embedded_newlines_in_name_reason_or_a_frame_are_flattened() {
        let ex = NsException {
            name: "NSGeneric\nException".into(),
            reason: "line one\nline two".into(),
            frames: vec!["frame with a\nnewline".into()],
        };
        let rendered = render(&ex);
        assert_eq!(rendered.lines().count(), 4, "one line each for name/reason/frame_count + 1 frame");
        let parsed = parse(&rendered).unwrap();
        assert_eq!(parsed.name, "NSGeneric Exception");
        assert_eq!(parsed.reason, "line one line two");
        assert_eq!(parsed.frames, vec!["frame with a newline"]);
    }

    #[test]
    fn a_foreign_or_truncated_stream_parses_to_none() {
        assert!(parse("not this format").is_none());
        assert!(parse("name=X\nreason=Y").is_none(), "missing frame_count");
        assert!(parse("name=X\nreason=Y\nframe_count=nope").is_none(), "unparseable count");
    }

    #[test]
    fn frame_count_short_of_the_actual_lines_takes_only_that_many() {
        let text = "name=X\nreason=Y\nframe_count=1\nframe a\nframe b\n";
        let parsed = parse(text).unwrap();
        assert_eq!(parsed.frames, vec!["frame a"]);
    }
}

#[cfg(feature = "as-library")]
mod global {
    use std::sync::atomic::{AtomicPtr, AtomicUsize, Ordering};

    static PTR: AtomicPtr<u8> = AtomicPtr::new(core::ptr::null_mut());
    static LEN: AtomicUsize = AtomicUsize::new(0);
    static EMPTY: [u8; 1] = [0];

    /// Copy the rendered payload into a leaked buffer once (mirrors `buildinfo`'s `set`/`cstr_ptr`).
    /// Called from `on_uncaught_exception`, which still runs in ordinary context — the later,
    /// constrained crash-time forwarding code then only does an atomic load and a `write`.
    pub(crate) fn set(text: &str) {
        let mut bytes: Vec<u8> = text.bytes().filter(|&b| b != 0).collect();
        bytes.push(0);
        let len = bytes.len() - 1;
        let leaked: &'static mut [u8] = Box::leak(bytes.into_boxed_slice());
        LEN.store(len, Ordering::SeqCst);
        PTR.store(leaked.as_mut_ptr(), Ordering::SeqCst);
    }

    /// NUL-terminated payload for the crash-time pipe field (an empty field if never set).
    #[cfg(target_os = "macos")]
    pub(crate) fn cstr_ptr() -> *const u8 {
        let p = PTR.load(Ordering::SeqCst);
        if p.is_null() { EMPTY.as_ptr() } else { p }
    }
}

#[cfg(feature = "as-library")]
pub(crate) use global::*;
