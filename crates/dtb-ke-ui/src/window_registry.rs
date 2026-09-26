//! Which of our windows are currently open, so opening one again focuses it instead of making a second.
//!
//! One `static` per window kind (`static OPEN: WindowRegistry<Kind> = WindowRegistry::new();`; a window
//! with a single instance uses `WindowRegistry<()>`). The state is process-global behind a `Mutex`, not
//! thread-local: it doesn't depend on which thread gpui happens to run `App` calls on, and an
//! `AnyWindowHandle` is plain `Copy` data.

use std::sync::{Mutex, MutexGuard, PoisonError};

use gpui_kit::{AnyWindowHandle, App};

pub struct WindowRegistry<K> {
    entries: Mutex<Vec<(K, AnyWindowHandle)>>,
}

impl<K: Copy + PartialEq> WindowRegistry<K> {
    pub const fn new() -> Self {
        Self { entries: Mutex::new(Vec::new()) }
    }

    fn entries(&self) -> MutexGuard<'_, Vec<(K, AnyWindowHandle)>> {
        // A panic while holding the lock can't leave the list inconsistent (it is only pushed / retained).
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The handle registered for `key`, if any (it may be stale — see [`focus`](Self::focus)).
    pub fn get(&self, key: K) -> Option<AnyWindowHandle> {
        self.entries().iter().find(|(k, _)| *k == key).map(|(_, h)| *h)
    }

    /// If a window is registered for `key` and still open, bring it forward and return `true`. A stale
    /// entry (its window is gone) is dropped and `false` returned, so the caller opens a fresh one.
    ///
    /// Existence is checked against `cx.windows()`, **not** the result of `handle.update`: when this
    /// runs from a menu action *while that very window is the one dispatching it*, the re-entrant
    /// `update` fails, and keying off that would drop the handle and open a second window.
    pub fn focus(&self, key: K, cx: &mut App) -> bool {
        let Some(handle) = self.get(key) else { return false };
        if cx.windows().contains(&handle) {
            handle.update(cx, |_, window, _| window.activate_window()).ok();
            return true;
        }
        self.remove(key);
        false
    }

    /// Register `handle` for `key`, replacing any earlier entry.
    pub fn insert(&self, key: K, handle: AnyWindowHandle) {
        let mut entries = self.entries();
        entries.retain(|(k, _)| *k != key);
        entries.push((key, handle));
    }

    pub fn remove(&self, key: K) {
        self.entries().retain(|(k, _)| *k != key);
    }
}

impl<K: Copy + PartialEq> Default for WindowRegistry<K> {
    fn default() -> Self {
        Self::new()
    }
}
