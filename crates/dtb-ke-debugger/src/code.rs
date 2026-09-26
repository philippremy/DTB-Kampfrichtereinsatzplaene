//! Finding the source text a frame's file/line refers to. Debug info stores the *build machine's*
//! absolute path, so the lookup tries, in order: the path as-is (same machine), the path re-rooted from
//! the recorded `workspace_root` onto a local checkout, and — for `/rustc/<hash>/library/…` — the
//! `rust-src` component of an installed toolchain. A remote (Codeberg at the dump's commit) source
//! slots in behind [`SourceLookup`] later.

use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Debug, Default)]
pub struct SourceRoots {
    /// The workspace root on the build machine (`workspace_root` of the build-info stream).
    pub build_root: Option<String>,
    /// Local checkouts to re-root onto (e.g. this repository).
    pub local_roots: Vec<PathBuf>,
}

#[derive(Clone, Debug)]
pub struct SourceText {
    pub path: PathBuf,
    /// How it was found, for the UI (`"same path"`, `"re-rooted onto …"`, `"rust-src"`).
    pub origin: String,
    pub text: Arc<str>,
}

impl SourceText {
    /// 1-based `line`, with `radius` lines of context on each side: `(first_line_number, lines)`.
    pub fn window(&self, line: u32, radius: u32) -> (u32, Vec<&str>) {
        let all: Vec<&str> = self.text.lines().collect();
        let first = line.saturating_sub(radius).max(1);
        let last = (line + radius).min(all.len() as u32);
        let slice = if first <= last {
            &all[(first - 1) as usize..last as usize]
        } else {
            &[][..]
        };
        (first, slice.to_vec())
    }
}

fn read(path: &Path, origin: impl Into<String>) -> Option<SourceText> {
    let bytes = std::fs::read(path).ok()?;
    Some(SourceText {
        path: path.to_path_buf(),
        origin: origin.into(),
        text: Arc::from(String::from_utf8_lossy(&bytes).into_owned()),
    })
}

/// `file` is the path from the debug info.
pub fn load(file: &str, roots: &SourceRoots) -> Option<SourceText> {
    let path = Path::new(file);
    if path.is_file() {
        return read(path, "same path");
    }
    if let Some(build) = roots.build_root.as_deref() {
        if let Some(rest) = file.strip_prefix(build) {
            let rest = rest.trim_start_matches(['/', '\\']);
            for root in &roots.local_roots {
                let candidate = root.join(rest);
                if candidate.is_file() {
                    return read(&candidate, format!("re-rooted onto {}", root.display()));
                }
            }
        }
    }
    if let Some(rest) = rustc_library_rest(file) {
        for toolchain in rustup_toolchains() {
            let candidate = toolchain.join("lib/rustlib/src/rust").join(rest);
            if candidate.is_file() {
                return read(&candidate, "rust-src of an installed toolchain");
            }
        }
    }
    None
}

/// `/rustc/<40 hex>/library/std/src/x.rs` → `library/std/src/x.rs`.
fn rustc_library_rest(file: &str) -> Option<&str> {
    let rest = file.strip_prefix("/rustc/")?;
    let (_hash, rest) = rest.split_once('/')?;
    Some(rest)
}

fn rustup_toolchains() -> Vec<PathBuf> {
    let home = std::env::var_os("RUSTUP_HOME")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME")
                .or_else(|| std::env::var_os("USERPROFILE"))
                .map(|h| PathBuf::from(h).join(".rustup"))
        });
    let Some(dir) = home.map(|h| h.join("toolchains")) else {
        return Vec::new();
    };
    std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dtbke-code-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn reroots_the_build_machine_path_onto_a_local_checkout() {
        let local = scratch("reroot");
        std::fs::create_dir_all(local.join("crates/x/src")).unwrap();
        std::fs::write(local.join("crates/x/src/lib.rs"), "a\nb\nc\nd\ne\n").unwrap();
        let roots = SourceRoots {
            build_root: Some("/ci/work/repo".into()),
            local_roots: vec![local.clone()],
        };

        let found = load("/ci/work/repo/crates/x/src/lib.rs", &roots).expect("re-rooted");
        assert!(found.origin.starts_with("re-rooted"));
        let (first, lines) = found.window(3, 1);
        assert_eq!((first, lines), (2, vec!["b", "c", "d"]));
        assert!(load("/elsewhere/crates/x/src/lib.rs", &roots).is_none());
        let _ = std::fs::remove_dir_all(local);
    }

    #[test]
    fn window_clamps_at_both_ends() {
        let t = SourceText {
            path: "x".into(),
            origin: String::new(),
            text: Arc::from("1\n2\n3"),
        };
        assert_eq!(t.window(1, 5), (1, vec!["1", "2", "3"]));
        assert_eq!(t.window(3, 0), (3, vec!["3"]));
    }

    #[test]
    fn rustc_paths_lose_their_hash() {
        assert_eq!(
            rustc_library_rest("/rustc/abc123/library/std/src/panicking.rs"),
            Some("library/std/src/panicking.rs")
        );
        assert_eq!(rustc_library_rest("/home/x/lib.rs"), None);
    }
}
