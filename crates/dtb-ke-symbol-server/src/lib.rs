//! The symbol server: a debug-file archive keyed by Breakpad debug id.
//!
//! ```text
//! GET /v1/debug/{DEBUG_ID}?name=&commit=&target=   read token   → 200 the file / 404
//! PUT /v1/debug/{DEBUG_ID}?name=&commit=&target=   upload token → 201 stored / 200 replaced / 422 wrong file
//! GET /healthz                                      no auth
//! ```
//!
//! The debugger's `SymbolServerSource` is the client (`Authorization: Bearer <token>`). An upload is verified
//! before it is stored: the body must be a Mach-O / ELF / PE / PDB that actually carries the id in the URL, so
//! the archive never holds a file under the wrong name. TLS and the public-facing hardening belong to the
//! reverse proxy in front; this process listens on loopback.

pub mod auth;
pub mod config;
pub mod store;

use std::sync::Arc;

use axum::Router;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use futures_util::StreamExt as _;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio_util::io::ReaderStream;

use auth::{Limiter, Role};
use config::Config;
use store::{Meta, Store, normalise_id};

pub struct AppState {
    config: Config,
    store: Store,
    limiter: Limiter,
}

impl AppState {
    pub fn new(config: Config) -> std::io::Result<Arc<Self>> {
        let store = Store::open(&config.store)?;
        Ok(Arc::new(Self {
            config,
            store,
            limiter: Limiter::default(),
        }))
    }
}

pub fn router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/v1/debug/{id}", get(get_debug).put(put_debug))
        .with_state(state)
}

#[derive(Deserialize, Default)]
struct Hints {
    name: Option<String>,
    commit: Option<String>,
    target: Option<String>,
}

fn reject(status: StatusCode, message: &str) -> Response {
    (status, format!("{message}\n")).into_response()
}

fn denied(status: StatusCode) -> Response {
    let mut response = reject(status, status.canonical_reason().unwrap_or("denied"));
    if status == StatusCode::UNAUTHORIZED {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
    }
    response
}

async fn get_debug(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
    Query(hints): Query<Hints>,
    headers: HeaderMap,
) -> Response {
    if let Err(status) = auth::authorize(&state.config, &state.limiter, &headers, Role::Read) {
        return denied(status);
    }
    let Some(id) = normalise_id(&raw_id) else {
        return reject(StatusCode::BAD_REQUEST, "not a debug id");
    };
    let file = match tokio::fs::File::open(state.store.file_path(&id)).await {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            log::info!(
                "get {id}: not found (name={:?} commit={:?} target={:?})",
                hints.name,
                hints.commit,
                hints.target
            );
            return reject(StatusCode::NOT_FOUND, "unknown debug id");
        }
        Err(e) => {
            log::error!("get {id}: cannot open the archive file: {e}");
            return reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
        }
    };
    let len = match file.metadata().await {
        Ok(m) => m.len(),
        Err(e) => {
            log::error!("get {id}: cannot stat the archive file: {e}");
            return reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
        }
    };
    log::info!("get {id}: serving {len} bytes");
    let mut response = Response::new(Body::from_stream(ReaderStream::new(file)));
    let h = response.headers_mut();
    h.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/octet-stream"),
    );
    h.insert(header::CONTENT_LENGTH, HeaderValue::from(len));
    response
}

/// Deletes the scratch file unless the upload was committed.
struct Scratch(Option<std::path::PathBuf>);

impl Scratch {
    fn take(&mut self) -> std::path::PathBuf {
        self.0.take().expect("scratch already taken")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = std::fs::remove_file(path);
        }
    }
}

async fn put_debug(
    State(state): State<Arc<AppState>>,
    Path(raw_id): Path<String>,
    Query(hints): Query<Hints>,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if let Err(status) = auth::authorize(&state.config, &state.limiter, &headers, Role::Upload) {
        return denied(status);
    }
    let Some(id) = normalise_id(&raw_id) else {
        return reject(StatusCode::BAD_REQUEST, "not a debug id");
    };
    let max = state.config.max_upload_bytes;
    let declared = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > max) {
        return reject(StatusCode::PAYLOAD_TOO_LARGE, "file too large");
    }

    // Stream the body to a scratch file, capped at `max` even when no length was declared.
    let mut scratch = Scratch(Some(state.store.scratch_path()));
    let path = scratch.0.clone().expect("just set");
    let mut file = match tokio::fs::File::create(&path).await {
        Ok(f) => f,
        Err(e) => {
            log::error!("put {id}: cannot create a scratch file: {e}");
            return reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
        }
    };
    let mut size = 0u64;
    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let Ok(chunk) = chunk else {
            return reject(StatusCode::BAD_REQUEST, "upload interrupted");
        };
        size += chunk.len() as u64;
        if size > max {
            return reject(StatusCode::PAYLOAD_TOO_LARGE, "file too large");
        }
        if let Err(e) = file.write_all(&chunk).await {
            log::error!("put {id}: write failed: {e}");
            return reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
        }
    }
    if let Err(e) = file.flush().await {
        log::error!("put {id}: flush failed: {e}");
        return reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error");
    }
    drop(file);

    // Verify: the file must really carry this id.
    let verify_path = path.clone();
    let identities = tokio::task::spawn_blocking(move || dtb_ke_symid::identify(&verify_path))
        .await
        .unwrap_or_default();
    if !identities.iter().any(|i| i.breakpad() == id) {
        let found: Vec<String> = identities.iter().map(|i| i.breakpad()).collect();
        log::warn!("put {id}: rejected, the file identifies as {found:?}");
        return reject(
            StatusCode::UNPROCESSABLE_ENTITY,
            "the file does not carry this debug id",
        );
    }

    let meta = Meta {
        name: hints.name,
        commit: hints.commit,
        target: hints.target,
        size,
        uploaded_unix: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs()),
    };
    match state.store.commit(&id, &scratch.take(), &meta) {
        Ok(replaced) => {
            log::info!(
                "put {id}: stored {size} bytes ({})",
                if replaced { "replaced" } else { "new" }
            );
            if replaced {
                StatusCode::OK
            } else {
                StatusCode::CREATED
            }
            .into_response()
        }
        Err(e) => {
            log::error!("put {id}: cannot commit: {e}");
            reject(StatusCode::INTERNAL_SERVER_ERROR, "storage error")
        }
    }
}
