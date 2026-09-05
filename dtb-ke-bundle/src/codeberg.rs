//! Codeberg/Forgejo release management — the `codeberg` subcommand.
//!
//! Both CI workflows and `scripts/release.sh` need the same three operations
//! (replace a release, upload assets to it, assemble + publish the updater's
//! `manifest.json`), so they live here once instead of duplicated as
//! bash+curl+jq in the workflow YAML and again in the release script.
//!
//! Talks to the Gitea/Forgejo v1 API via `ureq` (rustls) for everything
//! JSON; the asset **upload** shells out to `curl -F` for its multipart
//! body. A hand-rolled `multipart/form-data` body sent via `ureq` was tried
//! instead (avoiding the CLI-tool dependency `sha256sum`/`shasum` and
//! `bash` had already broken for elsewhere in this crate — see
//! `manifest::sha256` and RUNNERS.md's Portability section) and **twice
//! failed against a real upload** — first a raw connection-abort on both
//! Linux and Windows, then (after adding `Expect: 100-continue`, the most
//! likely fix for that) an outright HTTP 400 — while `curl` has uploaded
//! real archives successfully on Linux, Windows, and macOS throughout this
//! project's CI work. Reverted on the user's explicit instruction rather
//! than keep debugging a hand-rolled multipart body against a live service
//! with no local way to reproduce the failure: `curl`'s presence here is
//! now an *empirically confirmed* fact about all three CI runners, not an
//! assumption — the general "no arbitrary CLI tools" policy (RUNNERS.md)
//! still holds, it just isn't worth re-litigating for this one call again.
//!
//! The asset name can contain non-ASCII (this project's `DISPLAY_NAME` has
//! "ä"), and that broke `curl`-based uploads too, but only on Windows — see
//! [`Client::upload_raw`]'s doc comment for the actual mechanism and
//! [`url_encode`] for the fix (percent-encode before it ever becomes a
//! `curl` argument).
//!
//! Auth: `CODEBERG_TOKEN` (a personal/repo access token with `write:repository`
//! scope), read once in [`Client::from_env`].

use std::path::Path;
use std::process::Command;

use serde::Deserialize;

const API_BASE: &str = "https://codeberg.org/api/v1";
const OWNER: &str = "philippremy";
const REPO: &str = "DTB-Kampfrichtereinsatzplaene";

pub struct Client {
    token: String,
}

#[derive(Deserialize)]
struct ReleaseResponse {
    id: u64,
}

#[derive(Deserialize)]
struct AssetResponse {
    id: u64,
    name: String,
    browser_download_url: String,
}

impl Client {
    pub fn from_env() -> Result<Self, String> {
        let token =
            std::env::var("CODEBERG_TOKEN").map_err(|_| "CODEBERG_TOKEN is not set".to_string())?;
        Ok(Self { token })
    }

    fn url(&self, path: &str) -> String {
        format!("{API_BASE}/repos/{OWNER}/{REPO}{path}")
    }

    fn auth(
        &self,
        req: ureq::RequestBuilder<ureq::typestate::WithoutBody>,
    ) -> ureq::RequestBuilder<ureq::typestate::WithoutBody> {
        req.header("Authorization", &format!("token {}", self.token))
    }

    /// The release for `tag`, if one exists (`None` on a 404).
    fn find_by_tag(&self, tag: &str) -> Result<Option<ReleaseResponse>, String> {
        let url = self.url(&format!("/releases/tags/{tag}"));
        match self.auth(ureq::get(&url)).call() {
            Ok(mut resp) => resp
                .body_mut()
                .read_json::<ReleaseResponse>()
                .map(Some)
                .map_err(|e| format!("parsing release {tag}: {e}")),
            Err(ureq::Error::StatusCode(404)) => Ok(None),
            Err(e) => Err(format!("looking up release {tag}: {e}")),
        }
    }

    fn delete_release(&self, id: u64) -> Result<(), String> {
        let url = self.url(&format!("/releases/{id}"));
        self.auth(ureq::delete(&url))
            .call()
            .map(|_| ())
            .map_err(|e| format!("deleting release {id}: {e}"))
    }

    /// Delete any existing **release** for `tag` (a re-run's leftovers), then
    /// create a fresh one referencing `target` (a commit SHA or branch name).
    ///
    /// Deliberately does **not** touch the underlying git tag — for a real
    /// version tag it already exists exactly where `scripts/release.sh` put
    /// it, and for a rolling tag (`tip`/`latest`) the caller must force-move
    /// it with plain `git tag -f`/`git push -f` *before* calling this (a
    /// release API can create a tag that doesn't exist yet, but won't retarget
    /// one that does).
    ///
    /// Prints `release_id=<id>` to stdout and, when `$FORGEJO_OUTPUT` (or
    /// `$GITHUB_OUTPUT`, same thing) is set, appends `release_id=<id>` there
    /// too, so a workflow step can pick it up as `steps.<id>.outputs.release_id`.
    pub fn prepare(&self, opts: PrepareOptions) -> Result<u64, String> {
        if let Some(existing) = self.find_by_tag(opts.tag)? {
            eprintln!(
                "dtb-ke-bundle: replacing existing release {} (id {})",
                opts.tag, existing.id
            );
            self.delete_release(existing.id)?;
        }

        let body = serde_json::json!({
            "tag_name": opts.tag,
            "target_commitish": opts.target,
            "name": opts.title,
            "body": opts.notes.unwrap_or_default(),
            "draft": opts.draft,
            "prerelease": opts.prerelease,
        });
        let mut resp = self
            .token_post(&self.url("/releases"))
            .send_json(&body)
            .map_err(|e| format!("creating release {}: {e}", opts.tag))?;
        let release: ReleaseResponse = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("parsing created release: {e}"))?;

        eprintln!(
            "dtb-ke-bundle: created release {} (id {})",
            opts.tag, release.id
        );
        println!("release_id={}", release.id);
        if let Ok(out) = std::env::var("FORGEJO_OUTPUT").or_else(|_| std::env::var("GITHUB_OUTPUT"))
        {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(out)
            {
                let _ = writeln!(f, "release_id={}", release.id);
            }
        }
        Ok(release.id)
    }

    fn token_post(&self, url: &str) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
        ureq::post(url).header("Authorization", &format!("token {}", self.token))
    }

    /// Upload one file to `release_id` as an asset named after its file name.
    /// Returns the asset's `browser_download_url`. Shells to `curl` — see
    /// the module doc comment for why, after two failed attempts at a
    /// hand-rolled `ureq` multipart body.
    ///
    /// The asset name (`DISPLAY_NAME`-derived, so it can contain non-ASCII —
    /// "ä" for this project) is percent-encoded before it ever becomes a
    /// `curl` argument or lands in the request. Passing it raw worked on
    /// Linux (exact bytes reach `curl` unchanged via `execve`) but broke on
    /// Windows: `curl.exe`'s ANSI-CRT argument parsing round-trips a narrow
    /// command line through the process's ANSI code page, and while that
    /// round-trip is lossless for the *local file path* (the same code page
    /// converts it back on the `CreateFileA` side, so the upload always read
    /// the right file), the *outgoing request text* — the `?name=` query
    /// value and the `-F …;filename=` field — is sent as literal bytes with
    /// no second conversion, so "ä" (UTF-8 `C3 A4`) arrived as the single
    /// mangled byte `E4`: invalid UTF-8 on the wire, which Codeberg correctly
    /// rejected with HTTP 400. Percent-encoding turns the value into plain
    /// ASCII, which is immune to any code-page reinterpretation on either
    /// platform and is also just the standards-correct way to put arbitrary
    /// text in a URL query parameter to begin with.
    fn upload_raw(&self, release_id: u64, path: &Path) -> Result<String, String> {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("bad asset path: {}", path.display()))?;
        let encoded_name = url_encode(name);
        let url = format!(
            "{}?name={encoded_name}",
            self.url(&format!("/releases/{release_id}/assets"))
        );
        let out = Command::new("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "-X",
                "POST",
                "-H",
                &format!("Authorization: token {}", self.token),
                "-F",
                &format!("attachment=@{};filename={encoded_name}", path.display()),
                &url,
            ])
            .output()
            .map_err(|e| format!("spawning curl: {e}"))?;
        if !out.status.success() {
            return Err(format!(
                "uploading {name}: curl exited {} — {}",
                out.status,
                String::from_utf8_lossy(&out.stderr)
            ));
        }
        let asset: AssetResponse = serde_json::from_slice(&out.stdout)
            .map_err(|e| format!("parsing upload response for {name}: {e}"))?;
        Ok(asset.browser_download_url)
    }

    /// Upload every file in `files` to every release in `release_ids` (a
    /// version-tagged release CI also mirrors to the rolling `latest` release
    /// gets two ids here — see `UPDATER.md`). For each file, also computes its
    /// digest once and uploads a small `<name>.fragment.json` asset per
    /// release (`{name, url, size, digest}`) — [`Self::publish_manifest`]
    /// later merges these into the real `manifest.json` without having to
    /// re-download the (large) archives.
    pub fn upload(&self, release_ids: &[u64], files: &[std::path::PathBuf]) -> Result<(), String> {
        for path in files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("bad asset path: {}", path.display()))?
                .to_string();
            let size = std::fs::metadata(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len();
            let digest = crate::manifest::sha256(path)?;

            for &release_id in release_ids {
                let url = self.upload_raw(release_id, path)?;
                eprintln!(
                    "dtb-ke-bundle: uploaded {name} to release {release_id} ({})",
                    crate::util::human(size)
                );

                let fragment = serde_json::json!({ "name": name, "url": url, "size": size, "digest": format!("sha256:{digest}") });
                let fragment_path = std::env::temp_dir().join(format!("{name}.fragment.json"));
                std::fs::write(&fragment_path, serde_json::to_vec(&fragment).unwrap())
                    .map_err(|e| format!("writing {}: {e}", fragment_path.display()))?;
                self.upload_raw(release_id, &fragment_path)?;
                std::fs::remove_file(&fragment_path).ok();
            }
        }
        Ok(())
    }

    /// Like [`Self::upload`], but **without** a `.fragment.json` sidecar —
    /// for release assets that must never be considered by the self-update
    /// manifest (debug-info archives: `dtb-ke-bundle debug-info`'s output is
    /// exactly the kind of thing that must never look like a downloadable
    /// app update to `self_update`, which is what a fragment would make it
    /// look like to [`Self::publish_manifest`]).
    pub fn upload_plain(
        &self,
        release_ids: &[u64],
        files: &[std::path::PathBuf],
    ) -> Result<(), String> {
        for path in files {
            let name = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| format!("bad asset path: {}", path.display()))?
                .to_string();
            let size = std::fs::metadata(path)
                .map_err(|e| format!("{}: {e}", path.display()))?
                .len();
            for &release_id in release_ids {
                self.upload_raw(release_id, path)?;
                eprintln!(
                    "dtb-ke-bundle: uploaded {name} to release {release_id} ({}, no manifest fragment)",
                    crate::util::human(size)
                );
            }
        }
        Ok(())
    }

    /// Assemble `manifest.json` from every `*.fragment.json` asset on
    /// `release_id` (uploaded by [`Self::upload`], possibly across several
    /// parallel jobs/runners — this is the one place their output converges),
    /// upload it, remove the fragments, and optionally un-draft the release.
    pub fn publish_manifest(&self, release_id: u64, opts: ManifestOptions) -> Result<(), String> {
        let list_url = self.url(&format!("/releases/{release_id}/assets"));
        let mut resp = self
            .auth(ureq::get(&list_url))
            .call()
            .map_err(|e| format!("listing assets: {e}"))?;
        let assets: Vec<AssetResponse> = resp
            .body_mut()
            .read_json()
            .map_err(|e| format!("parsing asset list: {e}"))?;

        let mut manifest_assets = Vec::new();
        let mut fragment_ids = Vec::new();
        for asset in &assets {
            if !asset.name.ends_with(".fragment.json") {
                continue;
            }
            let mut fresp = self
                .auth(ureq::get(&asset.browser_download_url))
                .call()
                .map_err(|e| format!("downloading {}: {e}", asset.name))?;
            let text = fresp
                .body_mut()
                .read_to_string()
                .map_err(|e| format!("reading {}: {e}", asset.name))?;
            let value: serde_json::Value =
                serde_json::from_str(&text).map_err(|e| format!("parsing {}: {e}", asset.name))?;
            manifest_assets.push(value);
            fragment_ids.push(asset.id);
        }
        if manifest_assets.is_empty() {
            return Err(
                "no *.fragment.json assets found — did every build job's upload step run?".into(),
            );
        }

        let mut release = serde_json::json!({
            "version": opts.version,
            "assets": manifest_assets,
        });
        if let Some(date) = &opts.date {
            release["date"] = serde_json::Value::String(date.clone());
        }
        if let Some(notes) = &opts.notes_url {
            release["notes_url"] = serde_json::Value::String(notes.clone());
        }
        let manifest = serde_json::json!({ "schema": 1, "releases": [release] });

        let manifest_path = std::env::temp_dir().join("manifest.json");
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .map_err(|e| format!("writing {}: {e}", manifest_path.display()))?;

        // A rerun of a failed publish step must not fail on "asset already exists".
        if let Some(existing) = assets.iter().find(|a| a.name == "manifest.json") {
            let del_url = self.url(&format!("/releases/{release_id}/assets/{}", existing.id));
            self.auth(ureq::delete(&del_url)).call().ok();
        }
        self.upload_raw(release_id, &manifest_path)?;
        std::fs::remove_file(&manifest_path).ok();

        for id in fragment_ids {
            let del_url = self.url(&format!("/releases/{release_id}/assets/{id}"));
            self.auth(ureq::delete(&del_url))
                .call()
                .map_err(|e| format!("deleting fragment asset {id}: {e}"))?;
        }

        if opts.publish {
            let patch_url = self.url(&format!("/releases/{release_id}"));
            ureq::patch(&patch_url)
                .header("Authorization", &format!("token {}", self.token))
                .send_json(serde_json::json!({ "draft": false }))
                .map_err(|e| format!("publishing release {release_id}: {e}"))?;
            eprintln!("dtb-ke-bundle: release {release_id} published");
        }

        eprintln!(
            "dtb-ke-bundle: manifest.json written ({} asset(s))",
            manifest["releases"][0]["assets"].as_array().unwrap().len()
        );
        Ok(())
    }
}

/// Percent-encode `s` for use as a URL query-parameter value (RFC 3986
/// unreserved set only: `A-Za-z0-9-_.~`) — see [`Client::upload_raw`]'s doc
/// comment for why this matters here specifically. Deliberately hand-rolled
/// rather than pulled from a crate: it's a dozen lines and this file already
/// avoids adding dependencies for things this small.
fn url_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub struct PrepareOptions<'a> {
    pub tag: &'a str,
    pub target: &'a str,
    pub title: &'a str,
    pub notes: Option<String>,
    pub draft: bool,
    pub prerelease: bool,
}

pub struct ManifestOptions {
    pub version: String,
    pub date: Option<String>,
    pub notes_url: Option<String>,
    pub publish: bool,
}
