// Adapted from Zed's `gpui_tokio` (https://github.com/zed-industries/zed, crates/gpui_tokio).
// Copyright Zed Industries, Inc. Licensed under the Apache License, Version 2.0
// (https://www.apache.org/licenses/LICENSE-2.0). Changes: built against our `gpui-kit` gpui instead of
// Zed's workspace `gpui` (the upstream crate is git-only and would pull in a second copy of gpui), and
// `gpui_util::defer` replaced by the local `Defer`.
//
#![allow(dead_code)] // a faithful port; the viewer uses only part of the API

// Lets gpui code run futures that need a Tokio reactor (reqwest, wholesym's HTTP): the future runs on a
// Tokio pool and its result comes back through a gpui `Task`; dropping the `Task` aborts the future.

use std::future::Future;

use gpui_kit::{App, AppContext, Global, ReadGlobal, Task};

pub use tokio::task::JoinError;

/// Runs a closure on drop (the abort-on-cancel guard).
struct Defer(Option<Box<dyn FnOnce() + Send>>);

impl Drop for Defer {
    fn drop(&mut self) {
        if let Some(f) = self.0.take() {
            f();
        }
    }
}

fn defer(f: impl FnOnce() + Send + 'static) -> Defer {
    Defer(Some(Box::new(f)))
}

/// Initializes the Tokio wrapper using a new Tokio runtime with 2 worker threads.
///
/// If you need more threads (or access to the runtime outside of GPUI), you can create the runtime
/// yourself and pass a Handle to [`init_from_handle`].
pub fn init(cx: &mut App) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        // Since we now have two executors, let's try to keep our footprint small
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("Failed to initialize Tokio");

    let handle = runtime.handle().clone();
    cx.set_global(GlobalTokio {
        owned_runtime: Some(runtime),
        handle,
    });
}

/// Initializes the Tokio wrapper using a Tokio runtime handle.
#[allow(dead_code)]
pub fn init_from_handle(cx: &mut App, handle: tokio::runtime::Handle) {
    cx.set_global(GlobalTokio {
        owned_runtime: None,
        handle,
    });
}

struct GlobalTokio {
    owned_runtime: Option<tokio::runtime::Runtime>,
    handle: tokio::runtime::Handle,
}

impl Global for GlobalTokio {}

impl Drop for GlobalTokio {
    fn drop(&mut self) {
        if let Some(runtime) = self.owned_runtime.take() {
            runtime.shutdown_background();
        }
    }
}

pub struct Tokio {}

impl Tokio {
    /// Spawns the given future on Tokio's thread pool, and returns it via a GPUI task.
    /// Note that the Tokio task will be cancelled if the GPUI task is dropped.
    pub fn spawn<C, Fut, R>(cx: &C, f: Fut) -> Task<Result<R, JoinError>>
    where
        C: AppContext,
        Fut: Future<Output = R> + Send + 'static,
        R: Send + 'static,
    {
        cx.read_global(|tokio: &GlobalTokio, cx| {
            let join_handle = tokio.handle.spawn(f);
            let abort_handle = join_handle.abort_handle();
            let cancel = defer(move || {
                abort_handle.abort();
            });
            cx.background_spawn(async move {
                let result = join_handle.await;
                drop(cancel);
                result
            })
        })
    }

    /// Like [`Tokio::spawn`] for fallible futures; a join failure becomes the error.
    pub fn spawn_result<C, Fut, R>(cx: &C, f: Fut) -> Task<anyhow::Result<R>>
    where
        C: AppContext,
        Fut: Future<Output = anyhow::Result<R>> + Send + 'static,
        R: Send + 'static,
    {
        cx.read_global(|tokio: &GlobalTokio, cx| {
            let join_handle = tokio.handle.spawn(f);
            let abort_handle = join_handle.abort_handle();
            let cancel = defer(move || {
                abort_handle.abort();
            });
            cx.background_spawn(async move {
                let result = join_handle.await?;
                drop(cancel);
                result
            })
        })
    }

    pub fn handle(cx: &App) -> tokio::runtime::Handle {
        GlobalTokio::global(cx).handle.clone()
    }
}
