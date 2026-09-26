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

/// Like [`run`], with extra environment variables for the child.
pub fn run_with_env(program: &str, args: &[String], envs: &[(&str, &str)]) {
    let status = Command::new(program)
        .args(args)
        .envs(envs.iter().copied())
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

/// `crates/dtb-ke-bundle/../../` — i.e. the workspace root.
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("dtb-ke-bundle lives at crates/dtb-ke-bundle below the workspace root")
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
        Some(triple) => {
            cargo_target_root()
                .join(triple)
                .join(if release { "release" } else { "debug" })
        }
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

/// Where this build's debug-info sidecar lands, for the platforms that
/// produce one. `release`/`target` select which build; `name` only matters
/// for macOS/Linux (see below) — existence isn't checked here, that's the
/// caller's job, since a target that's never been built yet has no debug
/// info regardless of platform.
///
/// - **macOS**: `<name>.dSYM` next to the binary (a directory bundle;
///   `dsymutil` runs automatically) — `[profile.release] split-debuginfo =
///   "packed"` (workspace Cargo.toml) genuinely works here, confirmed
///   locally.
/// - **Linux**: `<name>.dwp` next to the binary (a single file) — `packed`
///   is stably supported per the rustc book; unverified locally (no Linux
///   linker on this dev machine).
/// - **Windows (gnullvm)**: `packed` does *not* work at all — confirmed by
///   testing `-C split-debuginfo=packed -Z unstable-options` on a real
///   nightly toolchain: accepted with no error, but the binary ends up with
///   no debug sections whatsoever, with or without `-C debuginfo=2` forcing
///   full debug info. Matches a real-world report of the identical
///   "unstable on this platform" wall on windows-gnu (a May-2025
///   users.rust-lang.org thread, closed unresolved) and the upstream
///   tracking issue acknowledging split-debuginfo is "effectively untested"
///   on Windows generally (rust-lang/rust#135531). A **real, separate PDB**
///   is produced anyway, by an entirely different, non-nightly mechanism:
///   `.cargo/config.toml` passes `-Wl,--pdb=<file>` through the
///   `x86_64-w64-mingw32-clang` linker driver to llvm-mingw's own `ld.lld`,
///   which genuinely emits one (`file` identifies it as "MSVC program
///   database ver 7.00" — confirmed real, not a guess). That path is a fixed
///   literal in `.cargo/config.toml` (rustflags there can't be templated,
///   and CI always overrides `$CARGO_TARGET_DIR` to something else anyway),
///   resolved relative to the workspace root — matching where the linker
///   actually runs, since every cargo invocation this project makes pins its
///   `current_dir` there (`Self::run`). `name` is ignored for this branch:
///   the filename is whatever's literally written into `.cargo/config.toml`,
///   independent of `RAW_BIN_NAME`. See RUNNERS.md's "Debug info" section
///   for the full investigation of both the packed dead end and the PDB fix.
pub fn debug_info_path(release: bool, target: Option<&str>, name: &str) -> Option<PathBuf> {
    let is_macos = match target {
        Some(triple) => triple.contains("apple-darwin") || triple.contains("apple-ios"),
        None => cfg!(target_os = "macos"),
    };
    let is_linux = match target {
        Some(triple) => triple.contains("linux"),
        None => cfg!(target_os = "linux"),
    };
    let dir = target_dir_for(release, target);
    if is_macos {
        Some(dir.join(format!("{name}.dSYM")))
    } else if is_linux {
        Some(dir.join(format!("{name}.dwp")))
    } else if target == Some("x86_64-pc-windows-gnullvm") {
        Some(workspace_root().join("dtb-ke-ui-x86_64.pdb"))
    } else if target == Some("aarch64-pc-windows-gnullvm") {
        Some(workspace_root().join("dtb-ke-ui-aarch64.pdb"))
    } else {
        None
    }
}

/// Where finished bundles land: `target/bundle/<profile>/`.
///
/// The app keeps the flat path (CI depends on it); any other product gets its own subfolder, so bundling one
/// never wipes the other's output (`bundle` starts from a fresh directory).
pub fn bundle_dir(release: bool) -> PathBuf {
    let dir = workspace_root()
        .join("target")
        .join("bundle")
        .join(if release { "release" } else { "debug" });
    if std::ptr::eq(crate::meta::p(), &crate::meta::APP) {
        dir
    } else {
        dir.join(crate::meta::p().slug)
    }
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
