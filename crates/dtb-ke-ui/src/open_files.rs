//! Cross-platform "open a `.dtbke` file from the OS" wiring — Finder
//! double-click / drag-onto-Dock (macOS), and the file-association command
//! line a freshly-launched process gets on Windows/Linux. Both funnel into
//! [`crate::app::open_paths`].
//!
//! macOS delivers this as a `file://` URL via gpui's `on_open_urls` platform
//! hook, fired both for a fresh launch (double-click while not running) and
//! for an already-running app (Dock drop / a second double-click). Windows
//! and Linux launch a **new process** per open, with the path as a plain
//! command-line argument — checked against gpui-pre-{windows,linux} 0.3.6:
//! both backends store an `on_open_urls` callback but neither ever invokes
//! it (no WM_COPYDATA / D-Bus activation handling behind it), so there is no
//! equivalent hook to rely on there.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use gpui_kit::{App, AsyncApp};

use crate::app;

/// URLs `on_open_urls` received while no `AsyncApp` was reachable — before [`ready`] ran (a launch-time
/// race on macOS: nothing guarantees the callback fires after our startup closure). Drained by `ready`.
/// Global, so the platform code handing us a URL may do so from any thread.
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

thread_local! {
    /// Set once by `ready`, on the thread gpui runs `App` calls on; lets a *later* `on_open_urls` call
    /// (app already running) reach into `cx` from outside it. `AsyncApp` is `!Send`, so it cannot live
    /// in a global — it is thread-local by necessity.
    static ASYNC_APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
}

fn pending() -> std::sync::MutexGuard<'static, Vec<PathBuf>> {
    PENDING.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn is_dtbke(path: &Path) -> bool {
    path.extension().is_some_and(|e| e.eq_ignore_ascii_case("dtbke"))
}

/// `.dtbke` paths among this process's command-line arguments — the
/// Windows/Linux file-association launch shape (see the module doc).
pub fn argv_paths() -> Vec<PathBuf> {
    std::env::args().skip(1).map(PathBuf::from).filter(|p| is_dtbke(p)).collect()
}

/// The `on_open_urls` callback (macOS only, functionally — see the module
/// doc). Register with `Application::on_open_urls` *before* `Application::
/// run`, the same shape as `Application::on_reopen` right next to it in
/// `main.rs`.
pub fn handle_open_urls(urls: Vec<String>) {
    let paths: Vec<PathBuf> =
        urls.iter().filter_map(|u| file_url_to_path(u)).filter(|p| is_dtbke(p)).collect();
    if paths.is_empty() {
        return;
    }
    match ASYNC_APP.with(|a| a.borrow().clone()) {
        Some(async_app) => async_app.update(|cx| app::open_paths(paths, cx)),
        None => pending().extend(paths),
    }
}

/// Call once from inside `Application::run`'s startup closure, after the
/// main window has (or would have) been opened: stashes an async handle for
/// any later `on_open_urls` delivery, then opens whatever is queued up —
/// anything `handle_open_urls` saw before this ran, plus the command-line
/// argument.
pub fn ready(cx: &mut App) {
    ASYNC_APP.with(|a| *a.borrow_mut() = Some(cx.to_async()));
    let mut queued = std::mem::take(&mut *pending());
    queued.extend(argv_paths());
    if !queued.is_empty() {
        app::open_paths(queued, cx);
    }
}

/// `"file:///Users/x/A%20File.dtbke"` → `/Users/x/A File.dtbke`. Only real
/// macOS `file://` URLs are expected here (see the module doc), but this
/// stays defensive rather than panicking on anything malformed.
pub fn file_url_to_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file://")?;
    let rest = rest.strip_prefix("localhost").unwrap_or(rest);
    Some(PathBuf::from(percent_decode(rest)))
}

fn percent_decode(s: &str) -> String {
    fn hex_nibble(b: u8) -> Option<u8> {
        match b {
            b'0'..=b'9' => Some(b - b'0'),
            b'a'..=b'f' => Some(b - b'a' + 10),
            b'A'..=b'F' => Some(b - b'A' + 10),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hi = bytes.get(i + 1).copied().and_then(hex_nibble);
            let lo = bytes.get(i + 2).copied().and_then(hex_nibble);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi << 4) | lo);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_spaces_and_unicode() {
        assert_eq!(
            file_url_to_path("file:///Users/x/A%20File%20%C3%A4.dtbke"),
            Some(PathBuf::from("/Users/x/A File ä.dtbke"))
        );
    }

    #[test]
    fn strips_localhost_authority() {
        assert_eq!(
            file_url_to_path("file://localhost/tmp/x.dtbke"),
            Some(PathBuf::from("/tmp/x.dtbke"))
        );
    }

    #[test]
    fn rejects_non_file_urls() {
        assert_eq!(file_url_to_path("https://example.com/x.dtbke"), None);
    }
}
