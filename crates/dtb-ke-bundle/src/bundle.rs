//! The `bundle` command: build the app for the host, then package it.

use std::path::PathBuf;

use crate::util::{self, built_binary_path, bundle_dir, fresh_dir};
use crate::{helper, icon, linux, macos, meta, windows};

/// The two Mach-O slices a `--universal` build merges with `lipo`. `pub(crate)`
/// — `debug_info::universal_dsym` merges the same two slices' `.dSYM`s the
/// same way.
pub(crate) const UNIVERSAL_TARGETS: [&str; 2] = ["x86_64-apple-darwin", "aarch64-apple-darwin"];

pub struct Options {
    /// Package the release binary (default) or the debug one (`--debug`).
    pub release: bool,
    /// Restrict which formats are produced (Linux only); `None` = all available.
    pub formats: Option<Vec<String>>,
    /// codesign identity for the macOS `.app` (`None` = ad-hoc `"-"`).
    pub sign: Option<String>,
    /// macOS only: build both `x86_64-apple-darwin` and `aarch64-apple-darwin`
    /// and merge them into one universal binary with `lipo` instead of building
    /// for the host's native arch alone. Requires both targets installed
    /// (`rustup target add x86_64-apple-darwin aarch64-apple-darwin`).
    pub universal: bool,
    /// Cross/slice-build for this target triple instead of the host's native
    /// one (e.g. building `aarch64-pc-windows-gnullvm` from an x86_64 Windows
    /// VM, or `aarch64-unknown-linux-gnu` from the x86_64 CachyOS runner).
    /// Mutually exclusive with `universal` (which already builds two targets
    /// itself). Requires the target installed and, for a cross target, its
    /// linker configured (`.cargo/config.toml` / `CARGO_TARGET_*_LINKER`).
    pub target: Option<String>,
}

/// Inputs every packager needs.
pub struct Context {
    /// The freshly built application binary.
    pub binary: PathBuf,
    /// `target/bundle/<profile>/` — where finished packages go.
    pub out_dir: PathBuf,
    pub formats: Option<Vec<String>>,
    pub sign: Option<String>,
    /// The `--target` triple this was built for (`None` = the host). Linux
    /// packaging derives its arch labels (`amd64`/`arm64`, …) from it.
    pub target: Option<String>,
    /// Whether an icon master was found and the icon cache is populated.
    pub have_icon: bool,
}

pub fn run(opts: Options) -> Result<(), String> {
    if opts.universal && !cfg!(target_os = "macos") {
        return Err("--universal only makes sense on macOS".into());
    }
    if opts.universal && opts.target.is_some() {
        return Err("--universal and --target are mutually exclusive".into());
    }

    // 1. Build the app (staging the crash helper first, like `build`).
    let binary = if opts.universal {
        build_universal(opts.release)?
    } else {
        let target = opts.target.as_deref();
        helper::stage(opts.release, target);
        let mut cargo = vec!["build".to_string(), "-p".into(), "dtb-ke-ui".into()];
        if opts.release {
            cargo.push("--release".into());
        }
        if let Some(triple) = target {
            cargo.push("--target".into());
            cargo.push(triple.to_string());
        }
        util::run("cargo", &cargo);

        let binary = built_binary_path(opts.release, target, meta::RAW_BIN_NAME);
        if !binary.exists() {
            return Err(format!("built binary not found at {}", binary.display()));
        }
        binary
    };

    // 2. Icons — already generated and committed to `assets/icons/generated/`
    // (see icon.rs's module doc): regeneration needs macOS/Xcode 26, so it
    // only ever happens via the standalone `icons` command, never here.
    let have_icon = icon::available();
    if !have_icon {
        eprintln!(
            "dtb-ke-bundle: no generated icons at {} — bundling without an icon \
             (run `cargo dtb-ke-bundle icons` on macOS and commit the result)",
            icon::generated_dir().display()
        );
    }

    // 3. Package for the host OS.
    let out_dir = bundle_dir(opts.release);
    fresh_dir(&out_dir).map_err(|e| format!("prepare {}: {e}", out_dir.display()))?;

    let cx = Context {
        binary,
        out_dir,
        formats: opts.formats,
        sign: opts.sign,
        target: opts.target,
        have_icon,
    };

    if cfg!(target_os = "macos") {
        macos::bundle(&cx)
    } else if cfg!(target_os = "windows") {
        windows::bundle(&cx)
    } else if cfg!(target_os = "linux") {
        linux::bundle(&cx)
    } else {
        Err("unsupported host OS — bundling runs on macOS, Windows or Linux".into())
    }
}

/// Build both macOS slices and merge them with `lipo` into one universal
/// binary. Each slice needs its *own* target-matched crash helper staged
/// first (`helper::stage`'s embedded helper is whatever was last staged — a
/// mismatched arch fails to `execve` at all), so the two builds run strictly
/// sequentially, never in parallel.
fn build_universal(release: bool) -> Result<PathBuf, String> {
    let mut slices = Vec::with_capacity(UNIVERSAL_TARGETS.len());
    for triple in UNIVERSAL_TARGETS {
        eprintln!("dtb-ke-bundle: building the {triple} slice …");
        helper::stage(release, Some(triple));

        let mut cargo = vec![
            "build".to_string(),
            "-p".into(),
            "dtb-ke-ui".into(),
            "--target".into(),
            triple.to_string(),
        ];
        if release {
            cargo.push("--release".into());
        }
        util::run("cargo", &cargo);

        let slice = built_binary_path(release, Some(triple), meta::RAW_BIN_NAME);
        if !slice.exists() {
            return Err(format!(
                "expected the {triple} slice at {}",
                slice.display()
            ));
        }
        slices.push(slice);
    }

    let merged_dir = util::workspace_root()
        .join("target/universal")
        .join(if release { "release" } else { "debug" });
    std::fs::create_dir_all(&merged_dir)
        .map_err(|e| format!("prepare {}: {e}", merged_dir.display()))?;
    let merged = merged_dir.join(meta::RAW_BIN_NAME);

    let mut lipo_args = vec![
        "-create".to_string(),
        "-output".into(),
        merged.to_string_lossy().into_owned(),
    ];
    lipo_args.extend(slices.iter().map(|p| p.to_string_lossy().into_owned()));
    util::run("lipo", &lipo_args);

    util::report(&merged);
    Ok(merged)
}

impl Context {
    /// Whether `format` should be produced (Linux `--formats` filter).
    pub fn wants(&self, format: &str) -> bool {
        self.formats
            .as_ref()
            .is_none_or(|list| list.iter().any(|f| f == format))
    }
}
