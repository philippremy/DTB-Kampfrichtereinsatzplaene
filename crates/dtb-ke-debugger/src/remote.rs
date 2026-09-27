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

/// Where downloads live between runs: `<OS cache dir>/de.philippremy.DTB-KE-Debugger/symbols`.
pub fn cache_dir() -> PathBuf {
    dirs::cache_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("de.philippremy.DTB-KE-Debugger")
        .join("symbols")
}

impl CacheSource {
    pub fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// `(files, bytes)` of everything below the root (a missing root is empty).
    pub fn usage(&self) -> (usize, u64) {
        walkdir::WalkDir::new(&self.root)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|e| e.file_type().is_file())
            .fold((0, 0), |(files, bytes), e| {
                (files + 1, bytes + e.metadata().map_or(0, |m| m.len()))
            })
    }

    /// Deletes every cached download (the root itself stays). Returns what was freed as `(files, bytes)`. Only ever
    /// removes the root's own children — symlinks are unlinked, never followed.
    pub fn clear(&self) -> std::io::Result<(usize, u64)> {
        let freed = self.usage();
        let entries = match std::fs::read_dir(&self.root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(freed),
            Err(e) => return Err(e),
        };
        for entry in entries {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                std::fs::remove_dir_all(entry.path())?;
            } else {
                std::fs::remove_file(entry.path())?;
            }
        }
        Ok(freed)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.root.join(id).join("debug-file")
    }

    /// A fresh scratch path *inside* the cache (so the final rename never crosses a filesystem) for a download of `id`.
    pub fn scratch_path(&self, id: &str) -> PathBuf {
        static DOWNLOADS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = DOWNLOADS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.root
            .join(format!(".download-{}-{n}-{id}.part", std::process::id()))
    }

    /// Moves a finished, verified download (a file made with [`Self::scratch_path`]) into place under `id`.
    pub fn adopt(&self, id: &str, scratch: &Path) -> std::io::Result<PathBuf> {
        let target = self.path_for(id);
        std::fs::create_dir_all(target.parent().expect("has parent"))?;
        std::fs::rename(scratch, &target)?;
        Ok(target)
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
    /// Told about the download in flight (bytes done / total), if the view wants a progress bar.
    progress: Option<std::sync::Arc<crate::progress::Progress>>,
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
            progress: None,
        })
    }

    /// Report each download's bytes done / total to `progress`.
    pub fn with_progress(mut self, progress: std::sync::Arc<crate::progress::Progress>) -> Self {
        self.progress = Some(progress);
        self
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

        let mut response = request.send().await.context("request failed")?;
        match response.status() {
            s if s == reqwest::StatusCode::NOT_FOUND => return Ok(None),
            s if !s.is_success() => bail!("server answered {s}"),
            _ => {}
        }

        // Stream into a scratch file inside the cache — never the whole (possibly gigabyte) body in memory — telling
        // the progress bar as the bytes arrive. The file only enters the cache once it has been verified.
        let total = response.content_length();
        let scratch = self.cache.scratch_path(&breakpad);
        std::fs::create_dir_all(scratch.parent().expect("has parent"))?;
        if let Some(p) = &self.progress {
            p.begin_transfer(
                &format!("{} from the symbol server", module.short_name()),
                total,
            );
        }
        let received = {
            use std::io::Write as _;
            let mut file = std::fs::File::create(&scratch)?;
            let mut received = 0u64;
            let result: anyhow::Result<u64> = async {
                while let Some(chunk) = response.chunk().await.context("download interrupted")? {
                    file.write_all(&chunk)?;
                    received += chunk.len() as u64;
                    if let Some(p) = &self.progress {
                        p.transfer_progress(received);
                    }
                }
                file.flush()?;
                Ok(received)
            }
            .await;
            if let Some(p) = &self.progress {
                p.end_transfer();
            }
            match result {
                Ok(received) => received,
                Err(err) => {
                    let _ = std::fs::remove_file(&scratch);
                    return Err(err);
                }
            }
        };

        // Verify before it counts: a wrong file never enters the cache.
        let verify_path = scratch.clone();
        let identity =
            tokio::task::spawn_blocking(move || identify_matching(&verify_path, id)).await?;
        let Some(identity) = identity else {
            let _ = std::fs::remove_file(&scratch);
            bail!("the server sent a file that does not match debug id {breakpad}");
        };
        let path = self.cache.adopt(&breakpad, &scratch)?;
        log::info!("downloaded {breakpad} ({received} bytes) into the cache");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cache_reports_its_size_and_clears_completely() {
        let dir = tempfile::tempdir().unwrap();
        let cache = CacheSource::new(dir.path().join("symbols"));
        assert_eq!(cache.usage(), (0, 0));
        assert_eq!(
            cache.clear().unwrap(),
            (0, 0),
            "a missing root is already empty"
        );

        cache.store("AAAA", &[1; 100]).unwrap();
        cache.store("BBBB", &[2; 50]).unwrap();
        std::fs::write(cache.root().join("stray.part"), [3; 10]).unwrap();
        assert_eq!(cache.usage(), (3, 160));

        assert_eq!(cache.clear().unwrap(), (3, 160));
        assert_eq!(cache.usage(), (0, 0));
        assert!(cache.root().is_dir(), "the root stays");
        assert_eq!(std::fs::read_dir(cache.root()).unwrap().count(), 0);
    }

    #[test]
    fn clearing_never_follows_a_symlink_out_of_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let outside = dir.path().join("outside");
        std::fs::create_dir(&outside).unwrap();
        std::fs::write(outside.join("keep.txt"), b"keep").unwrap();
        let cache = CacheSource::new(dir.path().join("symbols"));
        std::fs::create_dir(cache.root()).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, cache.root().join("link")).unwrap();
        cache.clear().unwrap();
        assert!(
            outside.join("keep.txt").exists(),
            "the target of a symlink must survive"
        );
    }
}
