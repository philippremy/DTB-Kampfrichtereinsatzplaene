//! Opening a `.dtbkedmp` from the OS: a Finder double-click / Dock drop (macOS delivers a `file://` URL through
//! gpui's `on_open_urls`) and the file-association command line on Windows / Linux (a fresh process per file,
//! the path as an argument — gpui's backends there never call `on_open_urls`; `main` reads the arguments).
//!
//! Mirrors `dtb-ke-ui`'s `open_files`, delivering into the single debugger window instead.

use std::cell::RefCell;
use std::path::PathBuf;
use std::sync::{Mutex, MutexGuard, PoisonError};

use crate::is_dump;
use dtb_ke_ui::open_files::file_url_to_path;
use gpui_kit::{App, AsyncApp};

use crate::ui;

/// URLs that arrived before [`ready`] ran (macOS may deliver one before the startup closure has run).
static PENDING: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

thread_local! {
    /// Set by [`ready`] on gpui's thread; lets a later `on_open_urls` call reach into `cx`.
    static ASYNC_APP: RefCell<Option<AsyncApp>> = const { RefCell::new(None) };
}

fn pending() -> MutexGuard<'static, Vec<PathBuf>> {
    PENDING.lock().unwrap_or_else(PoisonError::into_inner)
}

/// `Application::on_open_urls` callback.
pub fn handle_open_urls(urls: Vec<String>) {
    let paths: Vec<PathBuf> = urls
        .iter()
        .filter_map(|u| file_url_to_path(u))
        .filter(|p| is_dump(p))
        .collect();
    if paths.is_empty() {
        return;
    }
    match ASYNC_APP.with(|a| a.borrow().clone()) {
        Some(app) => app.update(|cx| deliver(paths, cx)),
        None => pending().extend(paths),
    }
}

/// Call once from the startup closure, after the window exists: opens whatever arrived early.
pub fn ready(cx: &mut App) {
    ASYNC_APP.with(|a| *a.borrow_mut() = Some(cx.to_async()));
    let queued = std::mem::take(&mut *pending());
    if !queued.is_empty() {
        deliver(queued, cx);
    }
}

/// One window, one dump: the last path wins.
fn deliver(paths: Vec<PathBuf>, cx: &mut App) {
    if let Some(path) = paths.into_iter().next_back() {
        ui::open(cx, Some(path), Vec::new());
        cx.activate(true);
    }
}
