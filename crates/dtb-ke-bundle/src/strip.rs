//! Linux and Windows-gnullvm release builds: keep the debug info out of the shipped binary *without losing it*.
//!
//! Neither platform has a usable "packed" sidecar (a Linux `.dwp` needs the skeleton in the stripped executable
//! and carries no debug id of its own; lld's `--pdb` for a DWARF build has no line numbers). So:
//!
//! 1. cargo builds the release profile **unstripped**, DWARF embedded (`CARGO_PROFILE_RELEASE_STRIP=none`,
//!    `…_SPLIT_DEBUGINFO=off` — overrides for these builds only; the shared profile is untouched). The cargo
//!    artifact itself is never modified, so re-running `bundle` can never turn it into a stripped one.
//! 2. **Linux** — the classic objcopy split, on a copy of the artifact:
//!    `objcopy --only-keep-debug app app.debug` (a small standalone debug file; the ELF build-id note survives),
//!    `objcopy --strip-debug --strip-unneeded app`, `objcopy --add-gnu-debuglink=app.debug app` (the shipped binary
//!    keeps its name and records the debug file's name + CRC). `app.debug` is what `symbols upload` sends.
//! 3. **Windows-gnullvm** — the shipped binary is a stripped copy (`llvm-strip --strip-all`); the debug file is
//!    `llvm-objcopy --only-keep-debug`'s output **if** it verifies (same debug id, all DWARF bytes kept — llvm-objcopy's
//!    PE support for that option is not something to trust blindly), else the unstripped executable itself. The
//!    debug id is the PE CodeView GUID + age, which lld's MinGW driver writes by default (no `--pdb=` needed).
//! 4. The shipped binary and the debug file must carry the same debug id(s) as the artifact, or a crash dump
//!    could never be matched to the uploaded file — checked here, and a mismatch is an error.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::util;

/// Overrides for the cargo invocation of a split build.
pub const PROFILE_ENV: [(&str, &str); 2] = [
    ("CARGO_PROFILE_RELEASE_STRIP", "none"),
    ("CARGO_PROFILE_RELEASE_SPLIT_DEBUGINFO", "off"),
];

/// Whether this build takes the split path (release Linux, or an explicit Windows-gnullvm target).
pub fn splits_debug_info(target: Option<&str>, release: bool) -> bool {
    release && (util::is_linux_target(target) || is_windows_gnullvm(target))
}

fn is_windows_gnullvm(target: Option<&str>) -> bool {
    target.is_some_and(|t| t.contains("windows-gnullvm"))
}

/// The binutils-style tools to try for `base` (`strip` / `objcopy`), best first. A cross target prefers the
/// matching binutils; the `llvm-` variant reads any architecture; on Windows llvm-mingw ships `llvm-<base>` and
/// `<arch>-w64-mingw32-<base>`.
fn candidates(base: &str, target: Option<&str>, host_arch: &str) -> Vec<String> {
    let arch = target
        .and_then(|t| t.split('-').next())
        .unwrap_or(host_arch);
    if is_windows_gnullvm(target) {
        vec![
            format!("llvm-{base}"),
            format!("{arch}-w64-mingw32-{base}"),
            base.into(),
        ]
    } else if arch == host_arch {
        vec![base.into(), format!("llvm-{base}")]
    } else {
        vec![format!("{arch}-linux-gnu-{base}"), format!("llvm-{base}")]
    }
}

fn find_tool(base: &str, target: Option<&str>) -> Result<String, String> {
    let tools = candidates(base, target, std::env::consts::ARCH);
    tools
        .iter()
        .find(|t| {
            Command::new(t)
                .arg("--version")
                .output()
                .is_ok_and(|o| o.status.success())
        })
        .cloned()
        .ok_or_else(|| {
            format!(
                "no {base} tool found (tried {}) — install binutils or llvm",
                tools.join(", ")
            )
        })
}

fn run_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .status()
        .map_err(|e| format!("spawning {tool}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{tool} exited {status}"))
    }
}

fn ids(path: &Path) -> BTreeSet<String> {
    dtb_ke_symid::identify(path)
        .iter()
        .map(|i| i.breakpad())
        .collect()
}

/// `objcopy --only-keep-debug`, then check the result still is what the build's debug id and DWARF say it is.
fn extract_debug(
    objcopy: &str,
    unstripped: &Path,
    out: &Path,
    before: &BTreeSet<String>,
    dwarf_bytes: u64,
) -> Result<(), String> {
    run_tool(
        objcopy,
        &[
            "--only-keep-debug".as_ref(),
            unstripped.as_os_str(),
            out.as_os_str(),
        ],
    )?;
    if !before.is_empty() && ids(out) != *before {
        return Err(format!(
            "the debug file has debug id(s) {:?}, the build has {before:?}",
            ids(out)
        ));
    }
    let kept = util::debug_section_bytes(out);
    if kept != dwarf_bytes {
        return Err(format!(
            "the debug file keeps {kept} of the build's {dwarf_bytes} DWARF bytes"
        ));
    }
    Ok(())
}

/// Splits `unstripped` (cargo's own artifact, left untouched): returns the path of the binary to ship. The debug
/// file is [`util::split_debug_file`] — on Windows only if `llvm-objcopy` could make one from the PE file and it
/// verified; otherwise the debug file is `unstripped` itself.
pub fn split(unstripped: &Path, release: bool, target: Option<&str>) -> Result<PathBuf, String> {
    let dir = util::target_dir_for(release, target).join("stripped");
    std::fs::create_dir_all(&dir).map_err(|e| format!("prepare {}: {e}", dir.display()))?;
    let name = unstripped
        .file_name()
        .ok_or("the built binary has no file name")?;
    let shipped = dir.join(name);
    std::fs::copy(unstripped, &shipped)
        .map_err(|e| format!("copy {}: {e}", unstripped.display()))?;

    let before = ids(unstripped);
    let dwarf_bytes = util::debug_section_bytes(unstripped);
    if dwarf_bytes == 0 {
        eprintln!(
            "dtb-ke-bundle: WARNING {} carries no DWARF — there will be nothing to upload",
            unstripped.display()
        );
    }
    let debug_path = util::split_debug_file(release, target, &debug_stem(unstripped));
    let _ = std::fs::remove_file(&debug_path);

    let debug_file = if is_windows_gnullvm(target) {
        let strip = find_tool("strip", target)?;
        eprintln!("dtb-ke-bundle: stripping a copy with {strip}");
        run_tool(&strip, &["--strip-all".as_ref(), shipped.as_os_str()])?;
        match find_tool("objcopy", target).and_then(|objcopy| {
            extract_debug(&objcopy, unstripped, &debug_path, &before, dwarf_bytes)
        }) {
            Ok(()) => debug_path,
            Err(why) => {
                eprintln!(
                    "dtb-ke-bundle: no standalone debug file for the PE ({why}) — keeping the unstripped executable"
                );
                let _ = std::fs::remove_file(&debug_path);
                unstripped.to_owned()
            }
        }
    } else {
        let objcopy = find_tool("objcopy", target)?;
        eprintln!("dtb-ke-bundle: splitting the debug info with {objcopy}");
        extract_debug(&objcopy, unstripped, &debug_path, &before, dwarf_bytes)?;
        run_tool(
            &objcopy,
            &[
                "--strip-debug".as_ref(),
                "--strip-unneeded".as_ref(),
                shipped.as_os_str(),
            ],
        )?;
        let mut link = std::ffi::OsString::from("--add-gnu-debuglink=");
        link.push(&debug_path);
        run_tool(&objcopy, &[link.as_os_str(), shipped.as_os_str()])?;
        debug_path
    };

    if before.is_empty() {
        eprintln!(
            "dtb-ke-bundle: WARNING {} has no debug id (no ELF build-id / PE CodeView record) — its crash dumps \
             cannot be matched to the uploaded debug file",
            unstripped.display()
        );
    } else if ids(&shipped) != before {
        return Err(format!(
            "the shipped binary {} has debug id(s) {:?}, the build has {before:?} — dumps of it would not match the debug file",
            shipped.display(),
            ids(&shipped)
        ));
    }
    eprintln!(
        "dtb-ke-bundle: shipped binary {} bytes, debug file {} bytes",
        file_len(&shipped),
        file_len(&debug_file)
    );
    Ok(shipped)
}

/// The name `util::debug_info_path` looks the debug file up by: the binary's name without the `.exe` cargo adds.
fn debug_stem(binary: &Path) -> String {
    binary
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn file_len(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |m| m.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_release_linux_and_windows_gnullvm_builds_split() {
        assert!(splits_debug_info(Some("x86_64-unknown-linux-gnu"), true));
        assert!(splits_debug_info(Some("aarch64-unknown-linux-gnu"), true));
        assert!(splits_debug_info(Some("x86_64-pc-windows-gnullvm"), true));
        assert!(splits_debug_info(Some("aarch64-pc-windows-gnullvm"), true));
        assert!(
            !splits_debug_info(Some("x86_64-unknown-linux-gnu"), false),
            "debug builds are left alone"
        );
        assert!(!splits_debug_info(Some("aarch64-apple-darwin"), true));
        assert!(!splits_debug_info(Some("aarch64-apple-ios"), true));
        assert!(!splits_debug_info(Some("x86_64-pc-windows-msvc"), true));
    }

    #[test]
    fn the_right_tool_is_tried_first() {
        assert_eq!(
            candidates("objcopy", Some("x86_64-unknown-linux-gnu"), "x86_64"),
            ["objcopy", "llvm-objcopy"]
        );
        assert_eq!(
            candidates("objcopy", Some("aarch64-unknown-linux-gnu"), "x86_64"),
            ["aarch64-linux-gnu-objcopy", "llvm-objcopy"]
        );
        assert_eq!(
            candidates("strip", Some("aarch64-pc-windows-gnullvm"), "x86_64"),
            ["llvm-strip", "aarch64-w64-mingw32-strip", "strip"]
        );
    }

    #[test]
    fn the_debug_file_is_named_after_the_binary_without_exe() {
        assert_eq!(
            debug_stem(Path::new("t/release/dtb-ke-ui.exe")),
            "dtb-ke-ui"
        );
        assert_eq!(debug_stem(Path::new("t/release/dtb-ke-ui")), "dtb-ke-ui");
    }

    #[test]
    fn identical_files_have_identical_ids_which_is_what_split_verifies() {
        let exe = std::env::current_exe().unwrap();
        let ids_before = ids(&exe);
        assert!(!ids_before.is_empty());
        let dir = tempfile::tempdir().unwrap();
        let copy = dir.path().join("copy");
        std::fs::copy(&exe, &copy).unwrap();
        assert_eq!(ids(&copy), ids_before);
    }
}
