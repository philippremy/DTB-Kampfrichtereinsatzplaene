//! Embed the crash helper staged by `cargo dtb-ke-bundle`.
//!
//! `dtb-ke-bundle` builds the `dtb-ke-crashhandler` bin (with `--features
//! as-binary`, so its own build.rs run skips the staging below) and copies it to
//! `embedded/dtb-ke-crashhandler`. Here we just point the crate at it via the
//! `DTB_KE_CRASH_HELPER` env, which `src/lib/macos/mod.rs` `include_bytes!`s under
//! the `as-library` feature. If the staged file is missing (a plain `cargo build`
//! with no `dtb-ke-bundle` run) we point at a 0-byte placeholder and the runtime
//! disables capture (`InstallError::NoHelper`); the panic hook still installs.

use std::path::PathBuf;

fn main() {
    let staged = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("embedded/dtb-ke-crashhandler");
    println!("cargo:rerun-if-changed={}", staged.display());
    println!("cargo:rerun-if-changed=build.rs");

    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dtb-ke-crashhandler");

    // When cargo is building the helper bin itself (`--features as-binary` with no
    // `as-library`), the lib's macos module isn't compiled and nothing reads
    // `DTB_KE_CRASH_HELPER` — but we still emit it (pointing at a placeholder) so a
    // combined `as-binary,as-library` build (e.g. `cargo test`) compiles, and we
    // stay quiet about a missing staged helper in that case.
    let building_helper_bin = std::env::var_os("CARGO_FEATURE_AS_BINARY").is_some()
        && std::env::var_os("CARGO_FEATURE_AS_LIBRARY").is_none();

    match std::fs::metadata(&staged) {
        Ok(m) if m.len() > 0 => {
            std::fs::copy(&staged, &out).expect("copy staged crash helper");
        }
        _ => {
            if !building_helper_bin {
                println!(
                    "cargo:warning=dtb-ke-crash: no staged helper at {} — run `cargo dtb-ke-bundle build`; \
                     crash capture is disabled for this build",
                    staged.display()
                );
            }
            let _ = std::fs::write(&out, b"");
        }
    }

    println!("cargo:rustc-env=DTB_KE_CRASH_HELPER={}", out.display());
}
