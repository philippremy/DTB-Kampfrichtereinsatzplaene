//! The fine-grained git client against real servers. Needs the network, so it only runs when asked:
//! `DTB_KE_NETWORK_TESTS=1 cargo test -p dtb-ke-debugger --test git_network -- --nocapture`.

use crate::git::{Lookup, RemoteRepo};
use gix::ObjectId;

fn enabled() -> bool {
    let on = std::env::var_os("DTB_KE_NETWORK_TESTS").is_some();
    if !on {
        eprintln!("skipped: set DTB_KE_NETWORK_TESTS=1 to run the network tests");
    }
    on
}

fn scratch(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("dtbke-git-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn dir_size(path: &std::path::Path) -> u64 {
    walkdir::WalkDir::new(path)
        .into_iter()
        .filter_map(Result::ok)
        .filter_map(|e| e.metadata().ok())
        .filter(|m| m.is_file())
        .map(|m| m.len())
        .sum()
}

const OURS: &str = "https://codeberg.org/philippremy/DTB-Kampfrichtereinsatzplaene";
const COMMIT: &str = "46f19351149f8bd3ce5a095c513fd985d5f08a99";

#[test]
fn reads_one_file_from_our_repo_without_cloning_it() {
    if !enabled() {
        return;
    }
    let cache = scratch("ours");
    let repo = RemoteRepo::open(&cache, OURS).unwrap();
    let commit = ObjectId::from_hex(COMMIT.as_bytes()).unwrap();

    let Lookup::File(bytes) = repo
        .read_file(commit, "crates/dtb-ke-ui/src/debug.rs")
        .unwrap()
    else {
        panic!("not a file")
    };
    let text = String::from_utf8(bytes).unwrap();
    assert!(
        text.starts_with("//! Hidden developer options"),
        "{}",
        &text[..text.len().min(80)]
    );

    // Compare with what git itself says this file was at that commit (when git and this checkout are around).
    if let Ok(out) = std::process::Command::new("git")
        .args(["show", &format!("{COMMIT}:crates/dtb-ke-ui/src/debug.rs")])
        .output()
    {
        if out.status.success() {
            assert_eq!(out.stdout, text.as_bytes(), "differs from `git show`");
        }
    }

    let downloaded = dir_size(&cache);
    eprintln!("cache after one file: {downloaded} bytes");
    assert!(
        downloaded < 300_000,
        "a fine-grained read must not clone: {downloaded} bytes"
    );

    // A second file of the same commit re-uses what is there and stays small.
    let again = repo
        .read_file(commit, "crates/dtb-ke-ui/src/main.rs")
        .unwrap();
    assert!(matches!(again, Lookup::File(_)));
    eprintln!("cache after two files: {} bytes", dir_size(&cache));

    // Missing paths are a clean NotFound, not an error.
    assert_eq!(
        repo.read_file(commit, "crates/nope/src/lib.rs").unwrap(),
        Lookup::NotFound
    );
    let _ = std::fs::remove_dir_all(cache);
}

/// `url` of the submodule at `path`, from the `.gitmodules` blob (a plain INI; no need for more than this here).
fn submodule_url(gitmodules: &str, path: &str) -> Option<String> {
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

#[test]
fn follows_a_submodule_into_its_own_repository() {
    if !enabled() {
        return;
    }
    let cache = scratch("sub");
    let ours = RemoteRepo::open(&cache, OURS).unwrap();
    let commit = ObjectId::from_hex(COMMIT.as_bytes()).unwrap();

    let Lookup::File(gm) = ours.read_file(commit, ".gitmodules").unwrap() else {
        panic!(".gitmodules")
    };
    let url = submodule_url(&String::from_utf8(gm).unwrap(), "vendor/zed")
        .expect("vendor/zed in .gitmodules");
    assert_eq!(url, "https://github.com/philippremy/zed.git");

    let Lookup::Submodule {
        commit: sub_commit,
        rest,
    } = ours
        .read_file(commit, "vendor/zed/crates/gpui/src/app.rs")
        .unwrap()
    else {
        panic!("expected the path to enter the submodule")
    };
    assert_eq!(rest, "crates/gpui/src/app.rs");
    assert_eq!(
        sub_commit.to_string(),
        "145b10fd9011efe0fab86a0e7746cf04e3168046"
    );

    let zed = RemoteRepo::open(&cache, &url).unwrap();
    let Lookup::File(bytes) = zed.read_file(sub_commit, &rest).unwrap() else {
        panic!("file in the submodule")
    };
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("pub struct App"), "unexpected content");
    eprintln!("zed cache after one file: {} bytes", dir_size(&cache));
    assert!(
        dir_size(&cache) < 2_000_000,
        "zed is huge; a fine-grained read must not clone it"
    );
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn reads_the_rust_standard_library_at_the_compilers_commit() {
    if !enabled() {
        return;
    }
    let cache = scratch("rust");
    // The hash `rustc` records in `/rustc/<hash>/library/…` paths (from a real dump of this project).
    let commit = ObjectId::from_hex(b"14cae681329a63c622a6e1fbe1d30f9374bc51d8").unwrap();
    let repo = RemoteRepo::open(&cache, "https://github.com/rust-lang/rust").unwrap();
    let Lookup::File(bytes) = repo
        .read_file(commit, "library/std/src/panicking.rs")
        .unwrap()
    else {
        panic!("not a file")
    };
    assert!(String::from_utf8_lossy(&bytes).contains("panic_with_hook"));
    eprintln!("rust cache after one file: {} bytes", dir_size(&cache));
    assert!(
        dir_size(&cache) < 3_000_000,
        "rust-lang/rust is gigabytes; a fine-grained read must not clone it"
    );
    let _ = std::fs::remove_dir_all(cache);
}

#[test]
fn plans_fetch_our_files_submodule_files_std_files_and_crates() {
    if !enabled() {
        return;
    }
    use crate::build::BuildInfo;
    use crate::sources;
    let cache = scratch("plans");
    let root = "/Users/x/proj";
    let build = BuildInfo::from_text(&format!("workspace_root={root}\ncommit_full={COMMIT}\n"));

    // Our own file.
    let plan = sources::plan(
        &format!("{root}/crates/dtb-ke-ui/src/debug.rs"),
        Some(&build),
    )
    .unwrap();
    assert!(
        sources::fetch_blocking(&plan, &cache, true)
            .unwrap()
            .starts_with("//! Hidden developer options")
    );

    // A file in a submodule of ours (crossing into another repository).
    let plan = sources::plan(
        &format!("{root}/vendor/zed/crates/gpui/src/app.rs"),
        Some(&build),
    )
    .unwrap();
    assert!(
        sources::fetch_blocking(&plan, &cache, true)
            .unwrap()
            .contains("pub struct App")
    );

    // The Rust standard library.
    let plan = sources::plan(
        "/rustc/14cae681329a63c622a6e1fbe1d30f9374bc51d8/library/std/src/panicking.rs",
        None,
    )
    .unwrap();
    assert!(
        sources::fetch_blocking(&plan, &cache, true)
            .unwrap()
            .contains("panic_with_hook")
    );

    // A registry crate (a `.crate` download, one file extracted; the archive is then cached).
    let plan = sources::plan(
        "/Users/x/.cargo/registry/src/index.crates.io-abc123/itoa-1.0.11/src/lib.rs",
        None,
    )
    .unwrap();
    let text = sources::fetch_blocking(&plan, &cache, true).unwrap();
    assert!(text.contains("itoa"), "{}", &text[..text.len().min(80)]);
    assert!(cache.join("crates/itoa-1.0.11.crate").is_file());
    assert!(
        sources::fetch_blocking(
            &sources::plan(
                "/Users/x/.cargo/registry/src/index.crates.io-abc123/itoa-1.0.11/Cargo.toml",
                None
            )
            .unwrap(),
            &cache,
            true
        )
        .unwrap()
        .contains("[package]")
    );

    // Offline, everything fetched above is now served from the cache — and something new is refused, not fetched.
    let again = sources::plan(
        &format!("{root}/crates/dtb-ke-ui/src/debug.rs"),
        Some(&build),
    )
    .unwrap();
    assert!(
        sources::fetch_blocking(&again, &cache, false)
            .unwrap()
            .starts_with("//! Hidden developer options")
    );
    let never_seen = sources::plan(
        &format!("{root}/crates/dtb-ke-ui/src/toolbar.rs"),
        Some(&build),
    )
    .unwrap();
    let err = sources::fetch_blocking(&never_seen, &cache, false).unwrap_err();
    assert!(
        err.downcast_ref::<crate::git::NeedsNetwork>()
            .is_some(),
        "{err:#}"
    );

    eprintln!(
        "total cache for four different sources: {} bytes",
        dir_size(&cache)
    );
    let _ = std::fs::remove_dir_all(cache);
}
