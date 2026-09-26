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
            origin: "cache".into(),
            in_dyld_cache: false,
        }))
    }
}

pub struct SymbolServerSource {
    base: String,
    token: Option<String>,
    client: reqwest::Client,
    cache: std::sync::Arc<CacheSource>,
}

impl SymbolServerSource {
    pub fn new(
        base: &str,
        token: Option<String>,
        cache: std::sync::Arc<CacheSource>,
    ) -> anyhow::Result<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("dtb-ke-debugger/", env!("CARGO_PKG_VERSION")))
            .timeout(std::time::Duration::from_secs(60))
            .build()?;
        Ok(Self {
            base: base.trim_end_matches('/').to_owned(),
            token,
            client,
            cache,
        })
    }
}

#[async_trait]
impl DebugFileSource for SymbolServerSource {
    fn name(&self) -> String {
        format!("symbol server ({})", self.base)
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
        let breakpad = id.breakpad().to_string();

        let mut request = self
            .client
            .get(format!("{}/v1/debug/{breakpad}", self.base));
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
        if let Some(token) = &self.token {
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
        let scratch =
            std::env::temp_dir().join(format!("dtbke-dl-{}-{breakpad}", std::process::id()));
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
            origin: format!("symbol server {}", self.base),
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
