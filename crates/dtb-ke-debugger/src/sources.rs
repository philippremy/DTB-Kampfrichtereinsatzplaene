//! Where the source of a stack frame's file lives, and how to fetch just that file — **only when the user asks**.
//!
//! Debug info records the *build machine's* path. From it, [`classify`] recovers what the file is:
//!
//! | path looks like | what it is | fetched from |
//! |---|---|---|
//! | `<workspace_root>/…` | our own code | the app's repository at the dump's commit (submodules followed) |
//! | `…/registry/src/index.crates.io-*/<name>-<ver>/…` | a crates.io dependency | `static.crates.io` (the `.crate`, one file extracted) |
//! | `…/git/checkouts/<repo>-<hash>/<rev>/…` | a cargo git dependency | its git remote at that revision (URL and full revision from the `Cargo.lock` at the dump's commit) |
//! | `/rustc/<hash>/library/…` | the Rust standard library | rust-lang/rust at that commit |
//!
//! Git fetches go through [`crate::git`] (pure-Rust `gix`, partial-clone requests: kilobytes, not the repository).
//! Nothing here runs by itself — the viewer shows a [`Plan`] and fetches only after the user confirms it.

use std::path::{Path, PathBuf};

use anyhow::{Context as _, anyhow, bail};
use gix::ObjectId;

use crate::build::BuildInfo;
use crate::git::{Lookup, RemoteRepo};

/// The app's own repository, for dumps too old to say (`repository=` in the build-info stream).
pub const DEFAULT_REPOSITORY: &str =
    "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene";
const RUST_REPOSITORY: &str = "https://github.com/rust-lang/rust";

/// What a debug-info file path refers to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Origin {
    Workspace {
        rel: String,
    },
    Registry {
        name: String,
        version: String,
        rel: String,
    },
    /// `short_rev` is the checkout directory's name (cargo uses the first 7 hex digits of the commit).
    GitDependency {
        short_rev: String,
        rel: String,
    },
    RustStd {
        commit: String,
        rel: String,
    },
}

fn looks_like_version(s: &str) -> bool {
    s.starts_with(|c: char| c.is_ascii_digit()) && s.contains('.')
}

/// `serde-1.0.200` → `("serde", "1.0.200")`; `foo-2fa-1.0.0-beta.1` → `("foo-2fa", "1.0.0-beta.1")`.
fn split_crate_dir(dir: &str) -> Option<(&str, &str)> {
    let mut end = dir.len();
    while let Some(i) = dir[..end].rfind('-') {
        if looks_like_version(&dir[i + 1..]) {
            return Some((&dir[..i], &dir[i + 1..]));
        }
        end = i;
    }
    None
}

pub fn classify(path: &str, workspace_root: Option<&str>) -> Option<Origin> {
    let path = path.replace('\\', "/");

    if let Some(rest) = path.strip_prefix("/rustc/") {
        let (hash, rel) = rest.split_once('/')?;
        if hash.len() == 40 && hash.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(Origin::RustStd {
                commit: hash.to_owned(),
                rel: rel.to_owned(),
            });
        }
    }
    if let Some(root) = workspace_root.map(|r| r.replace('\\', "/")) {
        if let Some(rel) = path
            .strip_prefix(root.trim_end_matches('/'))
            .and_then(|r| r.strip_prefix('/'))
        {
            return Some(Origin::Workspace {
                rel: rel.to_owned(),
            });
        }
    }
    if let Some((_, rest)) = path.split_once("/registry/src/") {
        // `index.crates.io-<hash>/<name>-<version>/<rel>`
        let mut parts = rest.splitn(3, '/');
        let (index, dir, rel) = (parts.next()?, parts.next()?, parts.next()?);
        if index.starts_with("index.crates.io-") {
            let (name, version) = split_crate_dir(dir)?;
            return Some(Origin::Registry {
                name: name.to_owned(),
                version: version.to_owned(),
                rel: rel.to_owned(),
            });
        }
    }
    if let Some((_, rest)) = path.split_once("/git/checkouts/") {
        // `<repo>-<hash>/<short rev>/<rel>`
        let mut parts = rest.splitn(3, '/');
        let (_repo, short, rel) = (parts.next()?, parts.next()?, parts.next()?);
        if short.len() >= 7 && short.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Some(Origin::GitDependency {
                short_rev: short.to_owned(),
                rel: rel.to_owned(),
            });
        }
    }
    None
}

/// What the viewer tells the user before fetching anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Plan {
    pub origin: Origin,
    /// The file, as the user would name it (`serde 1.0.200 · src/de.rs`).
    pub title: String,
    /// Where it would come from (`codeberg.org/… @ 46f1935`).
    pub source: String,
    /// A caveat worth showing next to the button, if any.
    pub caveat: Option<String>,
    /// For a workspace / git-dependency file: the repository and the full commit to read it from.
    repo: Option<(String, String)>,
}

fn host_and_path(url: &str) -> String {
    url.trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_end_matches(".git")
        .to_owned()
}

fn short(commit: &str) -> &str {
    &commit[..commit.len().min(7)]
}

/// The plan for `path`, or why there is none. `build` is the dump's build-info stream (older dumps have none).
pub fn plan(path: &str, build: Option<&BuildInfo>) -> Result<Plan, String> {
    let origin = classify(path, build.and_then(BuildInfo::workspace_root))
        .ok_or_else(|| "this file is not one the debugger knows how to fetch".to_owned())?;
    match &origin {
        Origin::Workspace { rel } => {
            let build = build.ok_or("the dump has no build info, so its commit is unknown")?;
            let commit = build
                .commit_full()
                .ok_or("the dump does not record the build's full commit")?;
            let url = build.repository().unwrap_or(DEFAULT_REPOSITORY).to_owned();
            Ok(Plan {
                title: rel.clone(),
                source: format!("{} @ {}", host_and_path(&url), short(commit)),
                caveat: build.dirty().then(|| "The build had uncommitted changes — the committed file may differ from what ran.".to_owned()),
                repo: Some((url, commit.to_owned())),
                origin,
            })
        }
        Origin::Registry { name, version, rel } => Ok(Plan {
            title: format!("{name} {version} · {rel}"),
            source: format!("crates.io · {name} {version}"),
            caveat: None,
            repo: None,
            origin,
        }),
        Origin::GitDependency { short_rev, rel } => {
            let build =
                build.ok_or("the dump has no build info, so its Cargo.lock cannot be consulted")?;
            let commit = build
                .commit_full()
                .ok_or("the dump does not record the build's full commit")?;
            let url = build.repository().unwrap_or(DEFAULT_REPOSITORY).to_owned();
            Ok(Plan {
                title: rel.clone(),
                source: format!("a git dependency at {short_rev} (found through Cargo.lock)"),
                caveat: None,
                // Here `repo` is the *app's* repository, whose Cargo.lock names the dependency's remote.
                repo: Some((url, commit.to_owned())),
                origin,
            })
        }
        Origin::RustStd { commit, rel } => Ok(Plan {
            title: rel.clone(),
            source: format!("{} @ {}", host_and_path(RUST_REPOSITORY), short(commit)),
            caveat: None,
            repo: None,
            origin,
        }),
    }
}

// ── executing a plan ─────────────────────────────────────────────────────────

/// `url` of the submodule at `path`, from the text of a `.gitmodules` (a plain INI).
pub fn submodule_url(gitmodules: &str, path: &str) -> Option<String> {
    let mut current_path = None;
    let mut current_url = None;
    for line in gitmodules.lines().map(str::trim) {
        if line.starts_with('[') {
            if current_path.as_deref() == Some(path) {
                return current_url;
            }
            (current_path, current_url) = (None, None);
        } else if let Some((k, v)) = line.split_once('=') {
            match k.trim() {
                "path" => current_path = Some(v.trim().to_owned()),
                "url" => current_url = Some(v.trim().to_owned()),
                _ => {}
            }
        }
    }
    (current_path.as_deref() == Some(path))
        .then_some(current_url)
        .flatten()
}

/// The remote (URL, full commit) of the git dependency whose checkout directory is `short_rev`, from a `Cargo.lock`.
pub fn git_dependency_in_lock(lock: &str, short_rev: &str) -> Option<(String, String)> {
    let table: toml::Table = lock.parse().ok()?;
    table.get("package")?.as_array()?.iter().find_map(|pkg| {
        let source = pkg.get("source")?.as_str()?.strip_prefix("git+")?;
        let (url_part, commit) = source.split_once('#')?;
        commit.starts_with(short_rev).then(|| {
            let url = url_part.split('?').next().unwrap_or(url_part).to_owned();
            (url, commit.to_owned())
        })
    })
}

/// Read `path` at `commit` in `url`, following submodules (`vendor/zed/…` continues in that repository).
fn read_in_repo(
    cache: &Path,
    url: &str,
    commit: &str,
    path: &str,
    depth: u8,
    online: bool,
) -> anyhow::Result<Vec<u8>> {
    if depth > 4 {
        bail!("too many nested submodules");
    }
    let id = ObjectId::from_hex(commit.as_bytes())
        .map_err(|_| anyhow!("{commit:?} is not a full commit id"))?;
    if !online && !RemoteRepo::exists(&cache.join("git"), url) {
        return Err(crate::git::NeedsNetwork.into());
    }
    let repo = RemoteRepo::open(&cache.join("git"), url)?.offline(!online);
    match repo.read_file(id, path)? {
        Lookup::File(bytes) => Ok(bytes),
        Lookup::NotFound => bail!(
            "{path} does not exist in {} at {}",
            host_and_path(url),
            short(commit)
        ),
        Lookup::Submodule {
            commit: sub_commit,
            rest,
        } => {
            let sub_path = path
                .strip_suffix(&rest)
                .unwrap_or(path)
                .trim_end_matches('/')
                .to_owned();
            let Lookup::File(gm) = repo.read_file(id, ".gitmodules")? else {
                bail!("no .gitmodules to resolve {sub_path}")
            };
            let url = submodule_url(&String::from_utf8_lossy(&gm), &sub_path).ok_or_else(|| {
                anyhow!("{sub_path} is a submodule, but .gitmodules does not name it")
            })?;
            read_in_repo(
                cache,
                &url,
                &sub_commit.to_string(),
                &rest,
                depth + 1,
                online,
            )
        }
    }
}

/// Fetch the plan's file. **Blocking** (git over the network): run it off the UI thread. With `online == false` it
/// reads only what an earlier fetch left in the cache and fails with [`crate::git::NeedsNetwork`] otherwise — that is
/// how the viewer shows a file it already has without asking again.
pub fn fetch_blocking(plan: &Plan, cache: &Path, online: bool) -> anyhow::Result<String> {
    let bytes = match (&plan.origin, &plan.repo) {
        (Origin::Workspace { rel }, Some((url, commit))) => {
            read_in_repo(cache, url, commit, rel, 0, online)?
        }
        (Origin::RustStd { commit, rel }, _) => {
            read_in_repo(cache, RUST_REPOSITORY, commit, rel, 0, online)?
        }
        (Origin::GitDependency { short_rev, rel }, Some((url, commit))) => {
            let lock = read_in_repo(cache, url, commit, "Cargo.lock", 0, online)
                .context("reading the Cargo.lock at the dump's commit")?;
            let (dep_url, dep_commit) =
                git_dependency_in_lock(&String::from_utf8_lossy(&lock), short_rev).ok_or_else(
                    || anyhow!("no git dependency at {short_rev} in that Cargo.lock"),
                )?;
            read_in_repo(cache, &dep_url, &dep_commit, rel, 0, online)?
        }
        (Origin::Registry { name, version, rel }, _) => {
            fetch_registry_file(cache, name, version, rel, online)?
        }
        _ => bail!("this plan has no repository to read from"),
    };
    String::from_utf8(bytes).or_else(|e| Ok(String::from_utf8_lossy(e.as_bytes()).into_owned()))
}

/// One file out of a crate's `.crate` (a gzipped tar) from `static.crates.io`; the archive is kept in the cache, so
/// the next file of the same crate costs nothing.
fn fetch_registry_file(
    cache: &Path,
    name: &str,
    version: &str,
    rel: &str,
    online: bool,
) -> anyhow::Result<Vec<u8>> {
    use std::io::Read;
    let archive_path: PathBuf = cache.join("crates").join(format!("{name}-{version}.crate"));
    if !archive_path.is_file() {
        if !online {
            return Err(crate::git::NeedsNetwork.into());
        }
        let url = format!("https://static.crates.io/crates/{name}/{name}-{version}.crate");
        let bytes = reqwest::blocking::get(&url)
            .and_then(|r| r.error_for_status())
            .and_then(|r| r.bytes())
            .with_context(|| format!("downloading {url}"))?;
        std::fs::create_dir_all(archive_path.parent().expect("has parent"))?;
        let tmp = archive_path.with_extension("part");
        std::fs::write(&tmp, &bytes)?;
        std::fs::rename(&tmp, &archive_path)?;
    }
    let file = std::fs::File::open(&archive_path)?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(file));
    let wanted = format!("{name}-{version}/{rel}");
    for entry in archive.entries()? {
        let mut entry = entry?;
        if entry.path()?.to_string_lossy() == wanted {
            let mut out = Vec::new();
            entry.read_to_end(&mut out)?;
            return Ok(out);
        }
    }
    bail!("{rel} is not in {name} {version}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "/Users/x/Desktop/DTB Kampfrichtereinsatzpläne";

    #[test]
    fn classifies_each_kind_of_path() {
        assert_eq!(
            classify(&format!("{ROOT}/crates/dtb-ke-ui/src/debug.rs"), Some(ROOT)),
            Some(Origin::Workspace {
                rel: "crates/dtb-ke-ui/src/debug.rs".into()
            })
        );
        assert_eq!(
            classify(
                &format!("{ROOT}/vendor/zed/crates/gpui/src/app.rs"),
                Some(ROOT)
            ),
            Some(Origin::Workspace {
                rel: "vendor/zed/crates/gpui/src/app.rs".into()
            })
        );
        assert_eq!(
            classify(
                "/Users/x/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/serde-1.0.200/src/de.rs",
                Some(ROOT)
            ),
            Some(Origin::Registry {
                name: "serde".into(),
                version: "1.0.200".into(),
                rel: "src/de.rs".into()
            })
        );
        assert_eq!(
            classify(
                "/Users/x/.cargo/git/checkouts/zed-0a1b2c3d4e5f6a7b/145b10f/crates/gpui/src/app.rs",
                None
            ),
            Some(Origin::GitDependency {
                short_rev: "145b10f".into(),
                rel: "crates/gpui/src/app.rs".into()
            })
        );
        assert_eq!(
            classify(
                "/rustc/14cae681329a63c622a6e1fbe1d30f9374bc51d8/library/std/src/panicking.rs",
                None
            ),
            Some(Origin::RustStd {
                commit: "14cae681329a63c622a6e1fbe1d30f9374bc51d8".into(),
                rel: "library/std/src/panicking.rs".into()
            })
        );
        assert_eq!(
            classify("/usr/lib/system/libsystem_c.dylib", Some(ROOT)),
            None
        );
        assert_eq!(classify("relative/path.rs", None), None);
    }

    #[test]
    fn crate_directories_split_at_the_version_even_with_dashes_and_prereleases() {
        assert_eq!(split_crate_dir("serde-1.0.200"), Some(("serde", "1.0.200")));
        assert_eq!(
            split_crate_dir("foo-2fa-1.0.0-beta.1"),
            Some(("foo-2fa", "1.0.0-beta.1"))
        );
        assert_eq!(
            split_crate_dir("tree-sitter-rust-0.24.2"),
            Some(("tree-sitter-rust", "0.24.2"))
        );
        assert_eq!(split_crate_dir("noversion"), None);
    }

    #[test]
    fn plans_need_a_commit_for_our_own_files_and_warn_about_dirty_builds() {
        let build = |text: &str| BuildInfo::from_text(text);
        let file = format!("{ROOT}/crates/x/src/lib.rs");

        // Without a build-info stream the workspace root is unknown, so the path is not even recognised as ours.
        assert!(plan(&file, None).is_err());
        let no_commit = build(&format!("workspace_root={ROOT}\n"));
        assert!(
            plan(&file, Some(&no_commit))
                .unwrap_err()
                .contains("full commit")
        );

        let ok = build(&format!(
            "workspace_root={ROOT}\ncommit_full={}\ndirty=true\n",
            "a".repeat(40)
        ));
        let p = plan(&file, Some(&ok)).unwrap();
        assert_eq!(
            p.source,
            "codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene @ aaaaaaa"
        );
        assert!(p.caveat.unwrap().contains("uncommitted"));

        // Crates and std need no build info at all.
        let p = plan(
            "/x/.cargo/registry/src/index.crates.io-abc/itoa-1.0.11/src/lib.rs",
            None,
        )
        .unwrap();
        assert_eq!(p.source, "crates.io · itoa 1.0.11");
        assert!(
            plan(
                "/rustc/14cae681329a63c622a6e1fbe1d30f9374bc51d8/library/core/src/fmt/mod.rs",
                None
            )
            .unwrap()
            .source
            .starts_with("github.com/rust-lang/rust @ 14cae68")
        );
    }

    #[test]
    fn gitmodules_urls_are_found_by_path() {
        let gm = "[submodule \"vendor/zed\"]\n\tpath = vendor/zed\n\turl = https://github.com/philippremy/zed.git\n[submodule \"vendor/taffy\"]\n\tpath = vendor/taffy\n\turl = https://github.com/philippremy/taffy.git\n";
        assert_eq!(
            submodule_url(gm, "vendor/taffy").as_deref(),
            Some("https://github.com/philippremy/taffy.git")
        );
        assert_eq!(
            submodule_url(gm, "vendor/zed").as_deref(),
            Some("https://github.com/philippremy/zed.git")
        );
        assert_eq!(submodule_url(gm, "vendor/none"), None);
    }

    #[test]
    fn a_git_dependency_is_found_in_cargo_lock_by_its_short_revision() {
        let lock = "[[package]]\nname = \"a\"\nversion = \"1.0.0\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\n\n[[package]]\nname = \"zed\"\nversion = \"0.1.0\"\nsource = \"git+https://github.com/zed-industries/zed?rev=145b10f#145b10fd9011efe0fab86a0e7746cf04e3168046\"\n";
        assert_eq!(
            git_dependency_in_lock(lock, "145b10f"),
            Some((
                "https://github.com/zed-industries/zed".into(),
                "145b10fd9011efe0fab86a0e7746cf04e3168046".into()
            ))
        );
        assert_eq!(git_dependency_in_lock(lock, "deadbee"), None);
    }
}
