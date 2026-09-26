//! Emits `cfg(has_app_icon)` when the debugger's flat icon has been generated (`cargo dtb-ke-bundle icons
//! --product debugger`), so `main.rs` can embed it for the About window on Windows/Linux without the build
//! depending on artwork that does not exist yet.

use std::path::Path;

fn main() {
    println!("cargo::rustc-check-cfg=cfg(has_app_icon)");
    let icon = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../assets/icons/debugger/generated/AppIcon.png");
    println!("cargo::rerun-if-changed={}", icon.display());
    if icon.exists() {
        println!("cargo::rustc-cfg=has_app_icon");
    }
}
