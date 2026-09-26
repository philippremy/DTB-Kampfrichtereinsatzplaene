//! Fine-grained access to a file in a git repository at a given commit, with `gix` (pure Rust — no `git` binary
//! needed) and **without downloading the repository**.
//!
//! A normal fetch takes the whole snapshot. This drives `gix-protocol`'s `fetch()` with a custom negotiator instead,
//! so each request can carry a partial-clone `filter` and `want` any object by id:
//!
//! 1. the **commit** alone — `want <commit>`, `deepen 1`, `filter tree:0` (a few hundred bytes);
//! 2. each **tree on the path**, one at a time — `want <tree>`, `filter tree:1` (just that directory listing);
//! 3. the **blob** — `want <blob>` (just that file).
//!
//! A source file costs about one request per path component plus one — kilobytes, from a repo that may be
//! gigabytes (rust-lang/rust, zed). Everything lands in a small bare repository per remote under the cache dir, so
//! the next file of the same commit re-uses the trees already fetched. The server must allow partial clones and
//! wants by object id (Codeberg / Forgejo, GitHub and GitLab do).
//!
//! Submodules are followed: a path that crosses a gitlink (`vendor/zed/…`) is reported as
//! [`Lookup::Submodule`], and [`crate::sources`] continues in the submodule's own repository at the pinned commit.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use anyhow::{Context as _, anyhow};
use gix::ObjectId;
use gix::protocol::fetch::{Arguments, Negotiate, negotiate};

/// Where a path in a commit leads.
#[derive(Debug, PartialEq, Eq)]
pub enum Lookup {
    File(Vec<u8>),
    /// The path enters a submodule: its commit, and the rest of the path inside it.
    Submodule {
        commit: ObjectId,
        rest: String,
    },
    NotFound,
}

pub struct RemoteRepo {
    url: String,
    repo: gix::Repository,
    /// Never touch the network: whatever is not already in the local repository is reported as
    /// [`NeedsNetwork`].
    offline: bool,
}

/// The requested object is not in the local cache and going online was not allowed.
#[derive(Debug)]
pub struct NeedsNetwork;

impl std::fmt::Display for NeedsNetwork {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("not in the local cache (a download is needed)")
    }
}
impl std::error::Error for NeedsNetwork {}

fn cache_dir_name(url: &str) -> String {
    // Readable + collision-proof enough: the host/path with separators flattened, plus a short checksum.
    let flat: String = url
        .trim_end_matches(".git")
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    let sum = url.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
        (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
    });
    format!("{flat}-{sum:08x}")
}

impl RemoteRepo {
    /// The bare repository for `url` under `cache_root` (created on first use).
    pub fn open(cache_root: &Path, url: &str) -> anyhow::Result<Self> {
        let dir = cache_root.join(cache_dir_name(url));
        let repo = if dir.join("HEAD").is_file() {
            gix::open(&dir).with_context(|| format!("opening {}", dir.display()))?
        } else {
            std::fs::create_dir_all(&dir)?;
            gix::init_bare(&dir).with_context(|| format!("creating {}", dir.display()))?
        };
        Ok(Self {
            url: url.to_owned(),
            repo,
            offline: false,
        })
    }

    /// Whether a repository for `url` was ever created under `cache_root` (so an offline read has a chance).
    pub fn exists(cache_root: &Path, url: &str) -> bool {
        cache_root.join(cache_dir_name(url)).join("HEAD").is_file()
    }

    /// Refuse to download: reads succeed only from what is already cached.
    pub fn offline(mut self, offline: bool) -> Self {
        self.offline = offline;
        self
    }

    /// The bytes of `path` at `commit`, fetching only what is missing.
    pub fn read_file(&self, commit: ObjectId, path: &str) -> anyhow::Result<Lookup> {
        if self.find(commit).is_none() {
            self.fetch(&[commit], "tree:0", true)
                .context("fetching the commit")?;
        }
        let commit_obj = self
            .find(commit)
            .ok_or_else(|| anyhow!("the server did not send commit {commit}"))?;
        let mut tree_id = commit_obj.try_into_commit()?.tree_id()?.detach();

        let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        for (i, part) in parts.iter().enumerate() {
            if self.find(tree_id).is_none() {
                self.fetch(&[tree_id], "tree:0", false)
                    .with_context(|| format!("fetching the directory listing for {part}"))?;
            }
            let tree = self
                .find(tree_id)
                .ok_or_else(|| anyhow!("tree {tree_id} was not delivered"))?
                .try_into_tree()?;
            let Some(entry) = tree
                .iter()
                .filter_map(Result::ok)
                .find(|e| e.filename() == part.as_bytes())
            else {
                return Ok(Lookup::NotFound);
            };
            let id = entry.oid().to_owned();
            let last = i + 1 == parts.len();
            match entry.mode().kind() {
                gix::object::tree::EntryKind::Commit => {
                    return Ok(Lookup::Submodule {
                        commit: id,
                        rest: parts[i + 1..].join("/"),
                    });
                }
                gix::object::tree::EntryKind::Tree if !last => tree_id = id,
                gix::object::tree::EntryKind::Blob
                | gix::object::tree::EntryKind::BlobExecutable
                    if last =>
                {
                    if self.find(id).is_none() {
                        self.fetch(&[id], "", false).context("fetching the file")?;
                    }
                    let blob = self
                        .find(id)
                        .ok_or_else(|| anyhow!("blob {id} was not delivered"))?;
                    return Ok(Lookup::File(blob.data.clone()));
                }
                _ => return Ok(Lookup::NotFound),
            }
        }
        Ok(Lookup::NotFound)
    }

    fn find(&self, id: ObjectId) -> Option<gix::Object<'_>> {
        self.repo.find_object(id).ok()
    }

    /// One `fetch` request: `wants` (any object ids), an optional partial-clone `filter`, optionally `deepen 1`.
    fn fetch(&self, wants: &[ObjectId], filter: &str, shallow_one: bool) -> anyhow::Result<()> {
        use gix::protocol::{fetch, handshake, transport};
        if self.offline {
            return Err(NeedsNetwork.into());
        }
        let interrupt = AtomicBool::new(false);
        let mut progress = gix::progress::Discard;

        let mut conn = transport::client::blocking_io::connect::connect(
            self.url.as_str(),
            transport::client::blocking_io::connect::Options {
                version: transport::Protocol::V2,
                ..Default::default()
            },
        )
        .map_err(|e| anyhow!("connecting to {}: {e:?}", self.url))?;

        let mut hs = handshake(
            &mut conn,
            transport::Service::UploadPack,
            // Public repositories only: no credentials are ever offered.
            |_action| Ok(None),
            Vec::new(),
            &mut progress,
        )
        .map_err(|e| anyhow!("handshake with {}: {e:?}", self.url))?;

        let mut negotiator = WantOnly {
            wants: wants.to_vec(),
            filter: filter.to_owned(),
        };
        let pack_dir = self.repo.objects.store_ref().path().join("pack");
        let shallow = if shallow_one {
            fetch::Shallow::DepthAtRemote(std::num::NonZeroU32::new(1).unwrap())
        } else {
            fetch::Shallow::NoChange
        };
        let object_hash = self.repo.object_hash();

        gix::protocol::fetch(
            &mut negotiator,
            |reader, progress, interrupt| -> Result<bool, gix::odb::pack::bundle::write::Error> {
                gix::odb::pack::Bundle::write_to_directory(
                    reader,
                    Some(&pack_dir),
                    progress,
                    interrupt,
                    None::<gix::odb::Cache<gix::odb::store::Handle<std::sync::Arc<gix::odb::Store>>>>,
                    object_hash,
                    gix::odb::pack::bundle::write::Options::default(),
                )?;
                Ok(true)
            },
            progress,
            &interrupt,
            fetch::Context {
                handshake: &mut hs,
                transport: &mut conn,
                user_agent: ("agent", Some("dtb-ke-debugger".to_owned())),
                trace_packetlines: false,
            },
            fetch::Options {
                shallow_file: self.repo.shallow_file(),
                shallow: &shallow,
                tags: fetch::Tags::None,
                reject_shallow_remote: false,
            },
        )
        .map_err(|e| anyhow!("fetch from {}: {e:?}", self.url))?
        .ok_or_else(|| anyhow!("the server had nothing to send"))?;
        Ok(())
    }
}

/// A negotiation of one round: no `have`s (we send only what we lack), explicit `want`s, an optional filter.
struct WantOnly {
    wants: Vec<ObjectId>,
    filter: String,
}

impl Negotiate for WantOnly {
    fn mark_complete_and_common_ref(&mut self) -> Result<negotiate::Action, negotiate::Error> {
        Ok(negotiate::Action::MustNegotiate {
            remote_ref_target_known: Vec::new(),
        })
    }

    fn add_wants(&mut self, arguments: &mut Arguments, _known: &[bool]) -> bool {
        if !self.filter.is_empty() {
            if !arguments.can_use_filter() {
                return false;
            }
            arguments.filter(&self.filter);
        }
        for id in &self.wants {
            arguments.want(id);
        }
        true
    }

    fn one_round(
        &mut self,
        _state: &mut negotiate::one_round::State,
        _arguments: &mut Arguments,
        _previous: Option<&gix::protocol::fetch::Response>,
    ) -> Result<(negotiate::Round, bool), negotiate::Error> {
        Ok((
            negotiate::Round {
                haves_sent: 0,
                in_vain: 0,
                haves_to_send: 0,
                previous_response_had_at_least_one_in_common: false,
            },
            // Done: ask for the pack right away.
            true,
        ))
    }
}

#[allow(dead_code)]
fn _unused(_: PathBuf) {}
