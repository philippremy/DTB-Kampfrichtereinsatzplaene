//! `cargo dtb-ke-bundle symbols upload` — puts this build's debug file on the symbol server, keyed by debug id,
//! so the debugger can symbolicate a crash dump from exactly this build (`dtb-ke-symbol-server`,
//! `PUT /v1/debug/{id}`).
//!
//! What is uploaded is the build's debug file (`util::debug_info_path`): macOS / iOS — the DWARF file inside the
//! `.dSYM` (a fat file for `--universal`, which identifies as one id per slice, so it is uploaded once per id);
//! Linux — the standalone `.debug` file from `objcopy --only-keep-debug`; Windows-gnullvm — the unstripped
//! executable the split build (`strip.rs`) keeps. A file that carries no
//! debug info (an old, already-stripped build) or no debug id is skipped with a warning rather than stored where it
//! could never symbolicate anything.
//!
//! **Best effort by default**: no server configured (forks, local runs) or a failed upload prints a warning and
//! leaves the job green. `--strict` (release builds) turns both into a failure, since a release without its symbols
//! cannot be symbolicated later.
//!
//! Environment: `DTB_KE_SYMBOLS_URL` (e.g. `https://symbols.example.net`) and `DTB_KE_SYMBOLS_UPLOAD_TOKEN`.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use crate::{debug_info, meta, util};

pub struct Options {
    pub release: bool,
    pub universal: bool,
    pub target: Option<String>,
    /// Fail the job when nothing is configured or an upload fails (release builds).
    pub strict: bool,
}

pub fn run(opts: Options) -> Result<(), String> {
    let env = |name: &str| std::env::var(name).ok().filter(|v| !v.trim().is_empty());
    let (Some(server), Some(token)) = (
        env("DTB_KE_SYMBOLS_URL"),
        env("DTB_KE_SYMBOLS_UPLOAD_TOKEN"),
    ) else {
        let message = "DTB_KE_SYMBOLS_URL / DTB_KE_SYMBOLS_UPLOAD_TOKEN not set";
        if opts.strict {
            return Err(format!("symbols upload: {message}"));
        }
        eprintln!("dtb-ke-bundle: symbols upload skipped — {message}");
        return Ok(());
    };
    let server = server.trim_end_matches('/').to_owned();

    let sidecar = if opts.universal {
        match debug_info::merged_universal_dsym(opts.release)? {
            Some(dir) => dir,
            None => return Ok(()),
        }
    } else {
        match util::debug_info_path(opts.release, opts.target.as_deref(), meta::p().raw_bin_name) {
            Some(path) if path.exists() => path,
            Some(path) => {
                eprintln!(
                    "dtb-ke-bundle: no debug file at {} — nothing to upload",
                    path.display()
                );
                return Ok(());
            }
            None => {
                eprintln!(
                    "dtb-ke-bundle: this platform produces no debug file — nothing to upload"
                );
                return Ok(());
            }
        }
    };

    let files = candidate_files(&sidecar)?;
    let commit = git_commit();
    let target = opts
        .target
        .clone()
        .or_else(|| opts.universal.then(|| "universal-apple-darwin".to_owned()));
    let mut uploaded = 0usize;
    let mut problems = 0usize;

    for file in &files {
        let identities = dtb_ke_symid::identify(file);
        if identities.is_empty() {
            eprintln!(
                "dtb-ke-bundle: WARNING {} has no debug id — skipped",
                file.display()
            );
            continue;
        }
        if !identities.iter().any(|i| i.has_debug_info) {
            eprintln!(
                "dtb-ke-bundle: WARNING {} carries no debug info (stripped?) — skipped",
                file.display()
            );
            continue;
        }
        for identity in identities {
            let id = identity.breakpad();
            match put(
                &server,
                &token,
                &id,
                file,
                commit.as_deref(),
                target.as_deref(),
            ) {
                Ok(status) => {
                    eprintln!(
                        "dtb-ke-bundle: ✔ symbol server: {id} ({status}) <- {}",
                        file.display()
                    );
                    uploaded += 1;
                }
                Err(e) => {
                    eprintln!("dtb-ke-bundle: WARNING symbol upload of {id} failed: {e}");
                    problems += 1;
                }
            }
        }
    }
    eprintln!("dtb-ke-bundle: symbols upload done — {uploaded} uploaded, {problems} failed");
    if opts.strict && (problems > 0 || uploaded == 0) {
        return Err("symbols upload: nothing (or not everything) reached the server".into());
    }
    Ok(())
}

/// The file(s) to identify: the file itself, or the DWARF files inside a `.dSYM`.
fn candidate_files(sidecar: &Path) -> Result<Vec<PathBuf>, String> {
    if sidecar.is_file() {
        return Ok(vec![sidecar.to_owned()]);
    }
    let dwarf = sidecar.join("Contents/Resources/DWARF");
    let entries =
        std::fs::read_dir(&dwarf).map_err(|e| format!("read {}: {e}", dwarf.display()))?;
    let mut files: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    files.sort();
    Ok(files)
}

/// `Ok(201)` stored, `Ok(200)` the server already had it (skipped).
///
/// A pre-flight `GET` comes first: the server answers a bad token before it reads the body, so a big upload
/// with the wrong token would otherwise die as a bare "broken pipe" instead of a 401 — and a file the archive
/// already holds (the same build, re-run) needs no second upload.
fn put(
    server: &str,
    token: &str,
    id: &str,
    file: &Path,
    commit: Option<&str>,
    target: Option<&str>,
) -> Result<u16, String> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_global(Some(Duration::from_secs(30 * 60)))
        .build()
        .into();
    let url = format!("{server}/v1/debug/{id}");
    let auth = format!("Bearer {token}");

    let probe = agent
        .head(&url)
        .header("Authorization", &auth)
        .call()
        .map_err(|e| format!("cannot reach the server: {e}"))?;
    match probe.status().as_u16() {
        200 => return Ok(200),
        404 => {}
        status @ (401 | 403) => {
            return Err(format!(
                "server answered {status} — check DTB_KE_SYMBOLS_UPLOAD_TOKEN"
            ));
        }
        status => return Err(format!("server answered {status} to the pre-flight check")),
    }

    let len = std::fs::metadata(file)
        .map_err(|e| format!("stat: {e}"))?
        .len();
    let body = std::fs::File::open(file).map_err(|e| format!("open: {e}"))?;
    let name = file
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("debug-file");
    let mut request = agent
        .put(&url)
        .header("Authorization", &auth)
        .header("Content-Length", &len.to_string())
        .query("name", name);
    if let Some(commit) = commit {
        request = request.query("commit", commit);
    }
    if let Some(target) = target {
        request = request.query("target", target);
    }
    let mut response = request.send(body).map_err(|e| e.to_string())?;
    let status = response.status().as_u16();
    if (200..300).contains(&status) {
        Ok(status)
    } else {
        let text = response.body_mut().read_to_string().unwrap_or_default();
        Err(format!("server answered {status}: {}", text.trim()))
    }
}

fn git_commit() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(util::workspace_root())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The uploader against the real server (in-process, loopback): identify → PUT → the archive has it.
    #[test]
    fn uploads_a_real_debug_file_to_the_real_server() {
        let store = tempfile::tempdir().unwrap();
        let config = dtb_ke_symbol_server::config::Config {
            listen: "127.0.0.1:0".parse().unwrap(),
            store: store.path().to_owned(),
            read_token: "read-token-0123456789".into(),
            upload_token: "upload-token-0123456789".into(),
            max_upload_bytes: 1 << 30,
        };
        let state = dtb_ke_symbol_server::AppState::new(config).unwrap();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
                .unwrap()
                .block_on(async move {
                    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
                    tx.send(listener.local_addr().unwrap()).unwrap();
                    axum::serve(listener, dtb_ke_symbol_server::router(state))
                        .await
                        .unwrap();
                });
        });
        let server = format!("http://{}", rx.recv().unwrap());

        let exe = std::env::current_exe().unwrap();
        let identity = dtb_ke_symid::identify(&exe)
            .into_iter()
            .next()
            .expect("the test executable has an id");
        let id = identity.breakpad();

        assert_eq!(
            put(
                &server,
                "upload-token-0123456789",
                &id,
                &exe,
                Some("abc"),
                Some("t")
            ),
            Ok(201)
        );
        assert!(store.path().join(&id).join("debug-file").is_file());
        let denied = put(&server, "wrong-token-0123456789", &id, &exe, None, None).unwrap_err();
        assert!(denied.contains("401"), "{denied}");
    }

    #[test]
    fn a_dsym_yields_its_dwarf_files_and_a_plain_file_itself() {
        let dir = tempfile::tempdir().unwrap();
        let dwarf = dir.path().join("X.dSYM/Contents/Resources/DWARF");
        std::fs::create_dir_all(&dwarf).unwrap();
        std::fs::write(dwarf.join("app"), b"x").unwrap();
        assert_eq!(
            candidate_files(&dir.path().join("X.dSYM")).unwrap(),
            vec![dwarf.join("app")]
        );
        let pdb = dir.path().join("a.pdb");
        std::fs::write(&pdb, b"x").unwrap();
        assert_eq!(candidate_files(&pdb).unwrap(), vec![pdb]);
    }
}
