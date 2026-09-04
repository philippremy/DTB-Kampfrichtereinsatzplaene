//! Build the out-of-process crash helper and stage it where
//! `crates/dtb-ke-crash/build.rs` `include_bytes!`s it.

use std::process::exit;

use crate::util::{built_binary_path, run, workspace_root};

/// Where `dtb-ke-crash/build.rs` looks for the helper to embed.
const STAGE: &str = "crates/dtb-ke-crash/embedded/dtb-ke-crashhandler";

/// Build `dtb-ke-crashhandler` (with `--features as-binary`, so its own build
/// script skips re-staging), copy it into place, and re-sign it on macOS.
///
/// `target`, when given, cross/slice-builds the helper for that triple instead
/// of the host's native one — required before each per-arch leg of a
/// [`crate::bundle::Options::universal`] macOS build, since the *next*
/// `dtb-ke-ui` build for that same triple `include_bytes!`s whatever is staged
/// here. A plain `build`/`bundle` (no universal flag) never passes this.
pub fn stage(release: bool, target: Option<&str>) {
    let root = workspace_root();

    let mut build = vec![
        "build".to_string(),
        "-p".into(),
        "dtb-ke-crash".into(),
        "--bin".into(),
        "dtb-ke-crashhandler".into(),
        "--features".into(),
        "as-binary".into(),
    ];
    if release {
        build.push("--release".into());
    }
    if let Some(triple) = target {
        build.push("--target".into());
        build.push(triple.to_string());
    }
    run("cargo", &build);

    let built = built_binary_path(release, target, "dtb-ke-crashhandler");
    let stage = root.join(STAGE);

    if !built.exists() {
        eprintln!("dtb-ke-bundle: expected helper at {}", built.display());
        exit(1);
    }
    std::fs::create_dir_all(stage.parent().unwrap()).ok();
    std::fs::copy(&built, &stage).unwrap_or_else(|e| {
        eprintln!(
            "dtb-ke-bundle: staging {} -> {} failed: {e}",
            built.display(),
            stage.display()
        );
        exit(1);
    });

    // An `execve`'d binary needs a valid signature on Apple Silicon. The
    // workspace `strip` *usually* keeps the linker's ad-hoc one, but re-applying
    // it is cheap and removes all doubt. (The `.app` codesign step later re-signs
    // the whole bundle; this keeps a plain `build` runnable too.)
    #[cfg(target_os = "macos")]
    run(
        "codesign",
        &[
            "--sign".into(),
            "-".into(),
            "--force".into(),
            stage.to_string_lossy().into_owned(),
        ],
    );

    eprintln!("dtb-ke-bundle: staged {}", stage.display());
}
