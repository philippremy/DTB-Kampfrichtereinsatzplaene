//! Debug files that are not on this machine: a persistent **cache** and the **symbol server**.
//!
//! The server is the eventual archive of every build's debug files (and executables), keyed by debug id;
//! the debugger never needs it twice for the same file, because every download lands in the cache first
//! and the cache sits ahead of the server in the resolver chain. The protocol is deliberately tiny:
//!
//! ```text
//! GET {base}/v1/debug/{DEBUG_ID}?name={debug file}&commit={sha}&target={triple}
//!     200 → the debug file (one file: a Mach-O DWARF image, a PDB, an ELF …), optionally gzip
//!     404 → not known
//! ```
//!
//! `DEBUG_ID` is the Breakpad spelling (upper-case hex, no dashes, age appended). `name`/`commit`/`target`
//! are hints for a server that can fetch or unpack on demand; the id alone identifies the file. A download
//! is verified against the requested id before it is cached or used.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use async_trait::async_trait;

use crate::build::BuildInfo;
use crate::identity::{ModuleRef, identify};
use crate::source::{DebugFileSource, FoundFile};

/// `<root>/<DEBUG_ID>/debug-file`, one verified file per id.
pub struct CacheSource {
    root: PathBuf,
}

impl CacheSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.root.join(id).join("debug-file")
    }

    /// Atomically add a file (temp + rename), so a crash mid-write never leaves a torn cache entry.
    pub fn store(&self, id: &str, bytes: &[u8]) -> anyhow::Result<PathBuf> {
        let target = self.path_for(id);
        std::fs::create_dir_all(target.parent().expect("has parent"))?;
        let tmp = target.with_extension("part");
        std::fs::write(&tmp, bytes)?;
        std::fs::rename(&tmp, &target)?;
        Ok(target)
    }
}

#[async_trait]
impl DebugFileSource for CacheSource {
    fn name(&self) -> String {
        format!("cache ({})", self.root.display())
    }

    async fn find(
        &self,
        module: &ModuleRef,
        _build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>> {
        let Some(id) = module.debug_id else {
            return Ok(None);
        };
        let path = self.path_for(&id.breakpad().to_string());
        let found = tokio::task::spawn_blocking(move || {
            // Re-verify on every hit: a cache entry that no longer matches its id is worse than a miss.
            identify(&path)
                .into_iter()
                .find(|i| i.debug_id == id)
                .map(|i| (path, i))
        })
        .await?;
        Ok(found.map(|(path, i)| FoundFile {
            path,
            has_debug_info: i.has_debug_info,
            has_symbols: i.has_symbols,
            origin: "cache".into(),
            in_dyld_cache: false,
        }))
    }
}

/// The server the debugger talks to: an address and an optional bearer token.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServerConfig {
    pub base: String,
    pub token: Option<String>,
}

impl ServerConfig {
    /// Normalises what a user typed: trims whitespace and trailing slashes, drops an empty token. `None` for an empty
    /// address; an error if it is not a full `http(s)://` address.
    pub fn parse(base: &str, token: &str) -> Result<Option<Self>, String> {
        let base = base.trim().trim_end_matches('/');
        if base.is_empty() {
            return Ok(None);
        }
        if !(base.starts_with("https://") || base.starts_with("http://")) {
            return Err("Enter the full address, e.g. https://symbols.example.net".into());
        }
        let token = token.trim();
        Ok(Some(Self {
            base: base.to_owned(),
            token: (!token.is_empty()).then(|| token.to_owned()),
        }))
    }
}

/// A shared, changeable [`ServerConfig`]: the resolver's [`SymbolServerSource`] reads it on every request, so the UI
/// can set or clear the server without rebuilding the resolver.
#[derive(Clone, Default)]
pub struct ServerHandle(std::sync::Arc<std::sync::Mutex<Option<ServerConfig>>>);

impl ServerHandle {
    pub fn get(&self) -> Option<ServerConfig> {
        self.0.lock().unwrap().clone()
    }

    pub fn set(&self, config: Option<ServerConfig>) {
        *self.0.lock().unwrap() = config;
    }
}

pub struct SymbolServerSource {
    server: ServerHandle,
    client: reqwest::Client,
    cache: std::sync::Arc<CacheSource>,
}

fn http_client() -> anyhow::Result<reqwest::Client> {
    Ok(reqwest::Client::builder()
        .user_agent(concat!("dtb-ke-debugger/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(60))
        .build()?)
}

impl SymbolServerSource {
    pub fn new(
        base: &str,
        token: Option<String>,
        cache: std::sync::Arc<CacheSource>,
    ) -> anyhow::Result<Self> {
        let server = ServerHandle::default();
        server.set(Some(ServerConfig {
            base: base.trim_end_matches('/').to_owned(),
            token,
        }));
        Self::with_handle(server, cache)
    }

    /// A source that follows `server`: unconfigured (a miss for everything) until the handle holds a config.
    pub fn with_handle(
        server: ServerHandle,
        cache: std::sync::Arc<CacheSource>,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            server,
            client: http_client()?,
            cache,
        })
    }
}

/// Whether `config` points at a reachable server that accepts its token: `GET /healthz`, then a lookup of an id
/// that cannot exist (a valid token answers 404 for it, a bad one 401/403). Returns a one-line description.
pub async fn check_server(config: &ServerConfig) -> Result<String, String> {
    let client = http_client().map_err(|e| e.to_string())?;
    let health = client
        .get(format!("{}/healthz", config.base))
        .send()
        .await
        .map_err(|e| format!("cannot reach {}: {e}", config.base))?;
    if !health.status().is_success() {
        return Err(format!(
            "{} answered {} to /healthz — is this the symbol server?",
            config.base,
            health.status()
        ));
    }
    let mut probe = client.get(format!("{}/v1/debug/{}", config.base, "0".repeat(33)));
    if let Some(token) = &config.token {
        probe = probe.bearer_auth(token);
    }
    let probe = probe
        .send()
        .await
        .map_err(|e| format!("request failed: {e}"))?;
    match probe.status().as_u16() {
        404 | 200 => Ok(format!(
            "Connected to {} — the token is accepted.",
            config.base
        )),
        401 => Err(
            "The server rejected the token (401) — check it, or enter it if none is set.".into(),
        ),
        403 => Err("The token is not allowed to read (403).".into()),
        429 => Err("Too many failed attempts from this address (429) — wait a few minutes.".into()),
        other => Err(format!("The server answered {other} to a lookup.")),
    }
}

#[async_trait]
impl DebugFileSource for SymbolServerSource {
    fn name(&self) -> String {
        match self.server.get() {
            Some(config) => format!("symbol server ({})", config.base),
            None => "symbol server (not configured)".into(),
        }
    }

    async fn find(
        &self,
        module: &ModuleRef,
        build: Option<&BuildInfo>,
    ) -> anyhow::Result<Option<FoundFile>> {
        let Some(id) = module.debug_id else {
            return Ok(None);
        };
        if module.is_system() {
            return Ok(None);
        }
        let Some(config) = self.server.get() else {
            return Ok(None);
        };
        let breakpad = id.breakpad().to_string();

        let mut request = self
            .client
            .get(format!("{}/v1/debug/{breakpad}", config.base));
        let mut query: Vec<(&str, &str)> = Vec::new();
        if let Some(name) = module.debug_file.as_deref() {
            query.push(("name", name));
        }
        if let Some(b) = build {
            if let Some(commit) = b.commit() {
                query.push(("commit", commit));
            }
            if let Some(target) = b.target() {
                query.push(("target", target));
            }
        }
        request = request.query(&query);
        if let Some(token) = &config.token {
            request = request.bearer_auth(token);
        }

        let response = request.send().await.context("request failed")?;
        match response.status() {
            s if s == reqwest::StatusCode::NOT_FOUND => return Ok(None),
            s if !s.is_success() => bail!("server answered {s}"),
            _ => {}
        }
        let bytes = response.bytes().await.context("download interrupted")?;

        // Verify before caching: identify a scratch copy, so a wrong file never enters the cache.
        static DOWNLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = DOWNLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let scratch =
            std::env::temp_dir().join(format!("dtbke-dl-{}-{n}-{breakpad}", std::process::id()));
        std::fs::write(&scratch, &bytes)?;
        let identity = identify_matching(&scratch, id);
        let _ = std::fs::remove_file(&scratch);
        let Some(identity) = identity else {
            bail!("the server sent a file that does not match debug id {breakpad}");
        };

        let path = self.cache.store(&breakpad, &bytes)?;
        log::info!(
            "downloaded {breakpad} ({} bytes) into the cache",
            bytes.len()
        );
        Ok(Some(FoundFile {
            path,
            has_debug_info: identity.has_debug_info,
            has_symbols: identity.has_symbols,
            origin: format!("symbol server {}", config.base),
            in_dyld_cache: false,
        }))
    }
}

fn identify_matching(
    path: &Path,
    id: samply_symbols::debugid::DebugId,
) -> Option<crate::identity::FileIdentity> {
    identify(path).into_iter().find(|i| i.debug_id == id)
}
