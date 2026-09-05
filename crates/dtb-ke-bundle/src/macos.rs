//! macOS `.app` bundle.
//!
//! Layout:
//!
//! ```text
//! DTB Kampfrichtereinsatzpläne.app/
//!   Contents/
//!     Info.plist
//!     PkgInfo
//!     MacOS/DTB Kampfrichtereinsatzpläne
//!     Resources/AppIcon.icns
//! ```
//!
//! The binary is self-contained (assets via RustEmbed, fonts embedded, the
//! crash helper `include_bytes!`d), so nothing else needs to go inside.

use std::path::Path;

use crate::bundle::Context;
use crate::util::{copy, fresh_dir, report, try_run, workspace_root};
use crate::{icon, meta};

pub fn bundle(cx: &Context) -> Result<(), String> {
    let app = cx.out_dir.join(format!("{}.app", meta::DISPLAY_NAME));
    let contents = app.join("Contents");
    fresh_dir(&contents).map_err(|e| format!("prepare {}: {e}", contents.display()))?;
    std::fs::create_dir_all(contents.join("MacOS")).map_err(io)?;
    std::fs::create_dir_all(contents.join("Resources")).map_err(io)?;

    // Executable — `meta::MACOS_EXECUTABLE_NAME` (ASCII), NOT the branded
    // `meta::DISPLAY_NAME`. codesign on macOS 26 cannot ad-hoc-sign an app
    // bundle whose main executable's *filename* contains a non-ASCII
    // character (the "ä" in "Kampfrichtereinsatzpläne"): `codesign --sign`
    // fails outright with "code object is not signed at all / In
    // subcomponent: …/MacOS/<name>", and even a bundle that does get signed
    // fails `codesign --verify --strict` ("a sealed resource is missing or
    // invalid"). Both go away with an ASCII executable name — verified
    // locally against this exact binary. The bundle *directory* keeps the
    // branded name, and so do `CFBundleName` / `CFBundleDisplayName` (what
    // Finder, the menu bar and Force-Quit show) — only the on-disk
    // executable, visible mainly in Activity Monitor / `ps`, changes.
    let exe = contents.join("MacOS").join(meta::MACOS_EXECUTABLE_NAME);
    copy(&cx.binary, &exe).map_err(io)?;
    make_executable(&exe)?;

    // Icon.
    let icon_file = if cx.have_icon && icon::icns_path().exists() {
        copy(&icon::icns_path(), &contents.join("Resources/AppIcon.icns")).map_err(io)?;
        Some("AppIcon")
    } else {
        None
    };

    // Info.plist + PkgInfo.
    std::fs::write(contents.join("Info.plist"), info_plist(icon_file)).map_err(io)?;
    std::fs::write(contents.join("PkgInfo"), b"APPL????").map_err(io)?;

    // License, for good measure.
    copy(
        &workspace_root().join("crates/dtb-ke-ui/assets/AGPL-3.0.txt"),
        &contents.join("Resources/LICENSE.txt"),
    )
    .ok();

    // Sign. Ad-hoc by default; a real identity via `--sign` (or $DTB_KE_SIGN_ID).
    let identity = cx
        .sign
        .clone()
        .or_else(|| std::env::var("DTB_KE_SIGN_ID").ok())
        .unwrap_or_else(|| "-".to_string());
    codesign(&app, &identity)?;

    report(&app);
    if identity == "-" {
        eprintln!(
            "dtb-ke-bundle: note — signed ad-hoc; Gatekeeper will quarantine this \
             on other Macs. Pass --sign \"Developer ID Application: …\" for distribution."
        );
    }

    // Optional .dmg — only if the user asked for it via --formats dmg.
    if cx.wants("dmg") && cx.formats.is_some() {
        make_dmg(cx, &app)?;
    }
    Ok(())
}

fn info_plist(icon_file: Option<&str>) -> String {
    let icon_entry = icon_file
        .map(|f| format!("\t<key>CFBundleIconFile</key>\n\t<string>{f}</string>\n"))
        .unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{display}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleExecutable</key>
	<string>{bin}</string>
{icon}	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleShortVersionString</key>
	<string>{short_version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>LSMinimumSystemVersion</key>
	<string>{min_os}</string>
	<key>LSApplicationCategoryType</key>
	<string>{category}</string>
	<key>NSHighResolutionCapable</key>
	<true/>
	<key>NSHumanReadableCopyright</key>
	<string>{copyright}</string>
	<key>NSSupportsAutomaticGraphicsSwitching</key>
	<true/>
</dict>
</plist>
"#,
        name = meta::DISPLAY_NAME,
        display = meta::DISPLAY_NAME,
        id = meta::IDENTIFIER,
        bin = meta::MACOS_EXECUTABLE_NAME,
        icon = icon_entry,
        short_version = meta::numeric_version(),
        version = meta::numeric_version(),
        min_os = meta::MACOS_MIN_VERSION,
        category = meta::MACOS_CATEGORY,
        copyright = xml_escape(meta::COPYRIGHT),
    )
}

fn codesign(app: &Path, identity: &str) -> Result<(), String> {
    // `--options runtime` only makes sense with a real identity (hardened
    // runtime + notarisation); ad-hoc stays plain so a local run isn't blocked.
    let app = app.to_string_lossy().into_owned();
    let mut args = vec!["--force", "--sign", identity, "--timestamp=none"];
    if identity != "-" {
        args.push("--options");
        args.push("runtime");
    }
    args.push(&app);
    try_run("codesign", &args, &workspace_root())
}

fn make_dmg(cx: &Context, app: &Path) -> Result<(), String> {
    let dmg = cx
        .out_dir
        .join(format!("{}-{}.dmg", meta::SLUG, meta::numeric_version()));
    crate::util::remove(&dmg).ok();
    try_run(
        "hdiutil",
        &[
            "create",
            "-volname",
            meta::DISPLAY_NAME,
            "-srcfolder",
            &app.to_string_lossy(),
            "-ov",
            "-format",
            "UDZO",
            &dmg.to_string_lossy(),
        ],
        &workspace_root(),
    )?;
    report(&dmg);
    Ok(())
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).map_err(io)?.permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).map_err(io)
}
#[cfg(not(unix))]
fn make_executable(_: &Path) -> Result<(), String> {
    Ok(())
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn io(e: std::io::Error) -> String {
    format!("{e}")
}
