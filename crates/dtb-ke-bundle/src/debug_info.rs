//! Packages each platform's separate debug-info file into a single
//! `.tar.gz`, kept entirely separate from `bundle`'s own packaging so debug
//! symbols never leak into an installed `.deb`/`.rpm`/`.AppImage` (those all
//! share one `/usr`-prefixed payload tree via `linux::stage_prefix`, which
//! this deliberately never touches).
//!
//! macOS's `.dSYM` bundle and Linux's `.dwp` file both come from
//! `[profile.release] split-debuginfo = "packed"` (workspace Cargo.toml).
//! Windows (gnullvm) doesn't work with that setting at all — see
//! [`crate::util::debug_info_path`]'s doc comment for the real testing that
//! established that — but gets a real, separate `.pdb` anyway, via a
//! completely different mechanism: `.cargo/config.toml` passing
//! `-Wl,--pdb=<file>` through to llvm-mingw's linker directly. Either way,
//! this module doesn't care which kind of file it's looking at —
//! [`util::debug_info_path`] resolves the right one, and [`tar_entry`] is
//! happy to archive a single file (Windows's `.pdb`, Linux's `.dwp`) or a
//! whole directory tree (macOS's `.dSYM`) alike.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::bundle::UNIVERSAL_TARGETS;
use crate::{meta, util};

pub struct Options {
    /// Package the release binary's sidecar (default) or the debug one.
    pub release: bool,
    /// macOS only: merge the two `--universal` slices' `.dSYM`s with `lipo`,
    /// the same way `bundle --universal` merges the slices' binaries.
    pub universal: bool,
    /// The specific target the sidecar was built for (`None` = host).
    /// Mutually exclusive with `universal`, matching `bundle::Options`.
    pub target: Option<String>,
    /// Where to write the resulting `.tar.gz`.
    pub out: PathBuf,
}

/// Finds this build's debug-info sidecar and archives it to `opts.out`.
/// Never returns an error for "there is nothing to package" — a platform
/// that doesn't produce a sidecar, or a target that hasn't been built yet,
/// just prints why and leaves `opts.out` unwritten. This is best-effort
/// extra material for offline symbolication, never something that should
/// fail a CI job.
pub fn run(opts: Options) -> Result<(), String> {
    if opts.universal {
        return universal_dsym(opts.release, &opts.out);
    }

    let target = opts.target.as_deref();
    let Some(path) = util::debug_info_path(opts.release, target, meta::RAW_BIN_NAME) else {
        eprintln!(
            "dtb-ke-bundle: this platform's split-debuginfo produces no sidecar file (see \
             RUNNERS.md) — nothing to package"
        );
        return Ok(());
    };
    if !path.exists() {
        eprintln!(
            "dtb-ke-bundle: expected a split-debuginfo sidecar at {} but it doesn't exist — \
             nothing to package",
            path.display()
        );
        return Ok(());
    }

    tar_entry(&path, &opts.out)?;
    util::report(&opts.out);
    Ok(())
}

/// macOS `--universal`: each slice's own `cargo build` (inside
/// `bundle::build_universal`) already produced its own per-arch `.dSYM` as a
/// side effect — this doesn't rebuild anything, just merges their inner
/// DWARF binaries with `lipo` (exactly like `build_universal` merges the two
/// slices' actual executables) and archives the result.
fn universal_dsym(release: bool, out: &Path) -> Result<(), String> {
    let mut dsyms = Vec::with_capacity(UNIVERSAL_TARGETS.len());
    for triple in UNIVERSAL_TARGETS {
        let path = util::debug_info_path(release, Some(triple), meta::RAW_BIN_NAME)
            .expect("apple-darwin triples always produce a .dSYM path");
        if !path.exists() {
            eprintln!(
                "dtb-ke-bundle: expected a {triple} .dSYM at {} but it doesn't exist — build \
                 with `bundle --universal` first",
                path.display()
            );
            return Ok(());
        }
        dsyms.push(path);
    }
    let [first, second] = dsyms.as_slice() else {
        unreachable!("UNIVERSAL_TARGETS has exactly 2 entries");
    };

    let merged_dir = util::workspace_root()
        .join("target/universal")
        .join(if release { "release" } else { "debug" })
        .join(format!("{}.dSYM", meta::RAW_DSYM_NAME));
    util::fresh_dir(&merged_dir).map_err(|e| format!("prepare {}: {e}", merged_dir.display()))?;
    copy_tree(first, &merged_dir)?;

    let dwarf_rel = Path::new("Contents/Resources/DWARF").join(meta::RAW_DSYM_NAME);
    let status = Command::new("lipo")
        .arg("-create")
        .arg("-output")
        .arg(merged_dir.join(&dwarf_rel))
        .arg(first.join(&dwarf_rel))
        .arg(second.join(&dwarf_rel))
        .status()
        .map_err(|e| format!("spawning lipo: {e}"))?;
    if !status.success() {
        return Err(format!("lipo exited {status}"));
    }

    tar_entry(&merged_dir, out)?;
    util::report(out);
    Ok(())
}

/// `tar -czf <out> -C <entry's parent> <entry's own name>` — plain, portable
/// flags only, unlike `archive::targz`'s GNU-tar-only long options (that one
/// only ever runs on the Linux host building `.deb`s; this runs on macOS's
/// BSD tar too). Works for a single file (Linux's `.dwp`) or a whole
/// directory tree (macOS's `.dSYM` bundle) alike — tar recurses either way.
fn tar_entry(entry: &Path, out: &Path) -> Result<(), String> {
    let parent = entry
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", entry.display()))?;
    let name = entry
        .file_name()
        .ok_or_else(|| format!("{} has no file name", entry.display()))?;
    if let Some(out_parent) = out.parent() {
        std::fs::create_dir_all(out_parent)
            .map_err(|e| format!("prepare {}: {e}", out_parent.display()))?;
    }
    let status = Command::new("tar")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(parent)
        .arg(name)
        .status()
        .map_err(|e| format!("spawning tar: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tar exited {status}"))
    }
}

/// Recursive directory copy — `std::fs::copy` (via `util::copy`) only ever
/// handles a single file. Only needed for the universal-merge staging step
/// above; `tar_entry` itself doesn't need this since `tar` recurses on its own.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    let mut stack = vec![from.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in
            std::fs::read_dir(&dir).map_err(|e| format!("reading {}: {e}", dir.display()))?
        {
            let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
            let path = entry.path();
            let rel = path
                .strip_prefix(from)
                .expect("walked path is always under `from`");
            let dest = to.join(rel);
            if path.is_dir() {
                std::fs::create_dir_all(&dest)
                    .map_err(|e| format!("mkdir {}: {e}", dest.display()))?;
                stack.push(path);
            } else {
                util::copy(&path, &dest).map_err(|e| format!("copy {}: {e}", path.display()))?;
            }
        }
    }
    Ok(())
}
