//! `cargo dtb-ke-bundle manifest` — build the `self_update` release manifest.
//!
//! CI runs this after building **and zipsign-signing** the per-platform archives
//! for a tagged release. It emits the schema-1 JSON the app's `manifest` backend
//! (and its own richer parse) consumes, then CI uploads it as a release asset on
//! both the version tag and a rolling `latest` tag so the app has a stable URL:
//!
//!   cargo dtb-ke-bundle manifest \
//!       --version 0.2.0 \
//!       --date 2026-09-10 \
//!       --notes-url https://codeberg.org/<owner>/<repo>/releases/tag/v0.2.0 \
//!       --base-url https://codeberg.org/<owner>/<repo>/releases/download/v0.2.0/ \
//!       --out manifest.json \
//!       dist/*.tar.gz dist/*.zip
//!
//! Each archive's file name **must contain the Rust target triple**
//! (`…-aarch64-apple-darwin.tar.gz`), which is how `self_update` matches the
//! asset to the running platform. The `url` is `<base-url><file name>` when
//! `--base-url` is given, else just the file name (a relative URL the app
//! resolves against the manifest's own URL — keep the manifest and the archives
//! in the same release).

use std::io::Read;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub struct Options {
    pub version: String,
    pub date: Option<String>,
    pub notes_url: Option<String>,
    pub base_url: Option<String>,
    pub out: PathBuf,
    pub archives: Vec<PathBuf>,
}

pub fn run(opts: Options) -> Result<(), String> {
    if opts.archives.is_empty() {
        return Err("no archives given".into());
    }

    let mut assets = String::new();
    for archive in &opts.archives {
        let name = archive
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| format!("bad archive path: {}", archive.display()))?;
        let size = std::fs::metadata(archive)
            .map_err(|e| format!("{}: {e}", archive.display()))?
            .len();
        let digest = sha256(archive)?;
        let url = match &opts.base_url {
            Some(base) => format!("{}{name}", base.trim_end_matches('/').to_string() + "/"),
            None => name.to_string(),
        };
        if !assets.is_empty() {
            assets.push_str(",\n");
        }
        assets.push_str(&format!(
            "        {{ \"name\": {name:?}, \"url\": {url:?}, \"size\": {size}, \"digest\": \"sha256:{digest}\" }}"
        ));
    }

    let mut release = format!("    {{\n      \"version\": {:?}", opts.version);
    if let Some(date) = &opts.date {
        release.push_str(&format!(",\n      \"date\": {date:?}"));
    }
    if let Some(notes) = &opts.notes_url {
        release.push_str(&format!(",\n      \"notes_url\": {notes:?}"));
    }
    release.push_str(&format!(
        ",\n      \"assets\": [\n{assets}\n      ]\n    }}"
    ));

    let json = format!("{{\n  \"schema\": 1,\n  \"releases\": [\n{release}\n  ]\n}}\n");
    std::fs::write(&opts.out, &json).map_err(|e| format!("{}: {e}", opts.out.display()))?;
    eprintln!(
        "dtb-ke-bundle: wrote {} ({} asset(s), version {})",
        opts.out.display(),
        opts.archives.len(),
        opts.version
    );
    Ok(())
}

/// The SHA-256 of a file as lower-case hex — pure Rust (the `sha2` crate),
/// streamed in fixed-size chunks rather than reading the whole (potentially
/// tens-of-MB release archive) file into memory at once. Deliberately not a
/// shelled `sha256sum`/`shasum` call: this runs unattended on Linux, macOS,
/// *and* Windows CI hosts, and neither of those tools is guaranteed to exist
/// on Windows.
pub(crate) fn sha256(path: &Path) -> Result<String, String> {
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("sha256 of {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("sha256 of {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
