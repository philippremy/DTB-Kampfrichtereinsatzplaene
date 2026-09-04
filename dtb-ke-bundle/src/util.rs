//! Shared helpers: process spawning, workspace paths, tool discovery.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio, exit};

/// Run a program in the workspace root; abort the whole task on failure.
pub fn run(program: &str, args: &[String]) {
    let status = Command::new(program)
        .args(args)
        .current_dir(workspace_root())
        .status()
        .unwrap_or_else(|e| {
            eprintln!("dtb-ke-bundle: failed to spawn {program}: {e}");
            exit(1);
        });
    if !status.success() {
        eprintln!("dtb-ke-bundle: {program} exited with {status}");
        exit(status.code().unwrap_or(1));
    }
}

/// Run a program in `dir`, returning `Err` with a message instead of aborting.
pub fn try_run(program: &str, args: &[&str], dir: &Path) -> Result<(), String> {
    let status = Command::new(program)
        .args(args)
        .current_dir(dir)
        .status()
        .map_err(|e| format!("failed to spawn {program}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{program} exited with {status}"))
    }
}

/// Whether `program` resolves on `PATH`.
pub fn have(program: &str) -> bool {
    Command::new(program)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success() || s.code().is_some())
        .unwrap_or(false)
}

/// `dtb-ke-bundle/../` — i.e. the workspace root.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("dtb-ke-bundle lives one level below the workspace root")
        .to_path_buf()
}

/// Where `cargo build` actually places its output root — `$CARGO_TARGET_DIR`
/// when set (a relative value resolves against `workspace_root()`, matching
/// every `util::run` invocation's `current_dir`), else the workspace's own
/// `target/`. **Every** self-hosted CI runner sets this to a fixed absolute
/// path outside the (ephemeral, per-job) checkout directory — Forgejo's
/// runner checks each job out into a fresh `$HOME/.cache/act/<rng>/…`, so
/// without this override there is no `target/` left to reuse between runs
/// and Tip's incremental-build caching (see RUNNERS.md) would silently do
/// nothing. This must be the *only* place that assumption is baked in — every
/// other path derived from the build output goes through `target_dir[_for]`.
fn cargo_target_root() -> PathBuf {
    match std::env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => {
            let dir = PathBuf::from(dir);
            if dir.is_absolute() {
                dir
            } else {
                workspace_root().join(dir)
            }
        }
        None => workspace_root().join("target"),
    }
}

/// `target/<profile>/` for the given profile.
pub fn target_dir(release: bool) -> PathBuf {
    cargo_target_root().join(if release { "release" } else { "debug" })
}

/// `target/<profile>/` (host build) or `target/<triple>/<profile>/` (a
/// `cargo build --target <triple>` cross/universal-slice build) — matches
/// where cargo actually places output in each case.
pub fn target_dir_for(release: bool, target: Option<&str>) -> PathBuf {
    match target {
        Some(triple) => cargo_target_root()
            .join(triple)
            .join(if release { "release" } else { "debug" }),
        None => target_dir(release),
    }
}

/// `".exe"` if `target` (or, when `None`, the *build host*) is Windows, else
/// `""` — cargo appends this to every binary it produces for a Windows
/// target, which every path built from a bare `BIN_NAME` needs to account for.
pub fn exe_suffix(target: Option<&str>) -> &'static str {
    let is_windows = match target {
        Some(triple) => triple.contains("windows"),
        None => cfg!(target_os = "windows"),
    };
    if is_windows { ".exe" } else { "" }
}

/// Where cargo places a binary named `name` for `target` (`None` = host),
/// `.exe`-suffixed as needed. The one place that combination is spelled out —
/// every caller that used to write `target_dir(..).join(BIN_NAME)` by hand
/// silently assumed a non-Windows host.
pub fn built_binary_path(release: bool, target: Option<&str>, name: &str) -> PathBuf {
    target_dir_for(release, target).join(format!("{name}{}", exe_suffix(target)))
}

/// Where finished bundles land: `target/bundle/<profile>/`.
pub fn bundle_dir(release: bool) -> PathBuf {
    workspace_root()
        .join("target")
        .join("bundle")
        .join(if release { "release" } else { "debug" })
}

/// Remove `path` if it exists (file or directory), ignoring "not found".
pub fn remove(path: &Path) -> std::io::Result<()> {
    let r = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match r {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

/// `mkdir -p` for a fresh, empty directory (removing any stale one first).
pub fn fresh_dir(path: &Path) -> std::io::Result<()> {
    remove(path)?;
    std::fs::create_dir_all(path)
}

/// Copy a file, creating parent directories.
pub fn copy(from: &Path, to: &Path) -> std::io::Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::copy(from, to).map(|_| ())
}

/// Human-readable byte size for log lines.
pub fn human(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut v = bytes as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    if u == 0 {
        format!("{bytes} B")
    } else {
        format!("{v:.1} {}", UNITS[u])
    }
}

/// Announce a finished artefact.
pub fn report(path: &Path) {
    let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
    // Directories report 0; only size real files.
    if path.is_dir() {
        eprintln!("dtb-ke-bundle: ✔ {}", path.display());
    } else {
        eprintln!("dtb-ke-bundle: ✔ {} ({})", path.display(), human(size));
    }
}
