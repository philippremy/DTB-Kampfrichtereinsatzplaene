//! Force a recompile whenever an embedded asset changes. `rust-embed` with
//! `debug-embed` bakes the files in at compile time, and cargo does not
//! otherwise notice edits under `assets/`.

fn main() {
    println!("cargo:rerun-if-changed=assets");
}
