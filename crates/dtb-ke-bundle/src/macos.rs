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
//!     Resources/AppIcon.icns   — pre-Tahoe fallback (CFBundleIconFile)
//!     Resources/Assets.car     — Liquid Glass icon (CFBundleIconName)
//! ```
//!
//! The binary is self-contained (assets via RustEmbed, fonts embedded, the
//! crash helper `include_bytes!`d), so nothing else needs to go inside.

use std::path::Path;
use std::process::Command;

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

    // Restamp the recorded macOS SDK version so AppKit gives the app the
    // macOS 26 interface (larger window controls, Liquid Glass) regardless of
    // which macOS SDK the linker on the build host had. No-op when it's
    // already ≥ the floor. See `meta::MACOS_SDK_FLOOR` / RUNNERS.md.
    ensure_min_sdk(&exe)?;

    // Icon. `AppIcon.icns` is the pre-Tahoe fallback (`CFBundleIconFile`);
    // `Assets.car` carries the actual Liquid Glass icon that Tahoe+ Finder
    // renders, looked up by name (`CFBundleIconName`) — both come from
    // `cargo dtb-ke-bundle icons` (macOS-only, committed to
    // `assets/icons/generated/`; see icon.rs's module doc).
    let icon_name = if cx.have_icon && icon::icns_path().exists() && icon::assets_car_path().exists()
    {
        copy(&icon::icns_path(), &contents.join("Resources/AppIcon.icns")).map_err(io)?;
        copy(&icon::assets_car_path(), &contents.join("Resources/Assets.car")).map_err(io)?;
        Some(icon::APP_ICON_NAME)
    } else {
        None
    };

    // `.dtbke` document-type icon (`UTTypeIconFile` below) — optional, same
    // shape as the app icon: bundle without one (a generic Finder file icon)
    // if it hasn't been committed yet.
    let doc_icon_name = if icon::doc_available() && icon::doc_icns_path().exists() {
        copy(&icon::doc_icns_path(), &contents.join("Resources/DocumentIcon.icns")).map_err(io)?;
        Some("DocumentIcon")
    } else {
        None
    };

    // Info.plist + PkgInfo.
    std::fs::write(contents.join("Info.plist"), info_plist(icon_name, doc_icon_name))
        .map_err(io)?;
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

fn info_plist(icon_name: Option<&str>, doc_icon_name: Option<&str>) -> String {
    // `CFBundleIconFile` — the legacy `.icns`, used pre-Tahoe (and by any
    // tooling that still only understands flat icon files). `CFBundleIconName`
    // — the Liquid Glass asset inside `Assets.car`, used by Tahoe+ Finder.
    // Both name the same on-disk basename here (`AppIcon`); Apple doesn't
    // require them to match (e.g. Blender ships `..._legacy.icns` /
    // `..._liquid_glass` as two different names) — see icon.rs's module doc.
    let icon_entry = icon_name
        .map(|f| {
            format!(
                "\t<key>CFBundleIconFile</key>\n\t<string>{f}</string>\n\
                 \t<key>CFBundleIconName</key>\n\t<string>{f}</string>\n"
            )
        })
        .unwrap_or_default();
    // `CFBundleAlternateNames` — short nicknames Spotlight/Siri also match,
    // since `DISPLAY_NAME` is long and carries a non-ASCII `ä`.
    let alternate_names = meta::MACOS_ALTERNATE_NAMES
        .iter()
        .map(|n| format!("\t\t<string>{}</string>\n", xml_escape(n)))
        .collect::<String>();

    // `.dtbke` file association: `CFBundleDocumentTypes` (Finder/Launch
    // Services — role, icon, "Open With" ranking) plus the
    // `UTExportedTypeDeclarations` that actually defines the UTI (we own
    // this type; nothing else declares it). `doc_icon_entry` is empty when
    // no document icon has been committed yet (see `icon::doc_available`) —
    // Finder then falls back to its generic document glyph.
    let doc_icon_entry = doc_icon_name
        .map(|f| format!("\t\t<key>UTTypeIconFile</key>\n\t\t<string>{f}</string>\n"))
        .unwrap_or_default();
    let document_types = format!(
        r#"	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
			<key>CFBundleTypeName</key>
			<string>{doc_name}</string>
			<key>CFBundleTypeRole</key>
			<string>Editor</string>
			<key>LSHandlerRank</key>
			<string>Owner</string>
			<key>LSItemContentTypes</key>
			<array>
				<string>{uti}</string>
			</array>
		</dict>
	</array>
	<key>UTExportedTypeDeclarations</key>
	<array>
		<dict>
			<key>UTTypeIdentifier</key>
			<string>{uti}</string>
			<key>UTTypeDescription</key>
			<string>{doc_name}</string>
			<key>UTTypeConformsTo</key>
			<array>
				<string>public.data</string>
			</array>
			<key>UTTypeTagSpecification</key>
			<dict>
				<key>public.filename-extension</key>
				<array>
					<string>{ext}</string>
				</array>
			</dict>
{doc_icon_entry}		</dict>
	</array>
"#,
        doc_name = xml_escape(meta::DOC_TYPE_NAME),
        uti = meta::DOC_UTI,
        ext = meta::DOC_EXTENSION,
        doc_icon_entry = doc_icon_entry,
    );

    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{display}</string>
	<key>CFBundleAlternateNames</key>
	<array>
{alternate_names}	</array>
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
	<key>CFBundleDevelopmentRegion</key>
	<string>de</string>
{document_types}	<key>LSMinimumSystemVersion</key>
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
        document_types = document_types,
        short_version = meta::numeric_version(),
        version = meta::numeric_version(),
        min_os = meta::MACOS_MIN_VERSION,
        category = meta::MACOS_CATEGORY,
        copyright = xml_escape(meta::COPYRIGHT),
    )
}

/// Ensure the binary's `LC_BUILD_VERSION` records an SDK ≥
/// [`meta::MACOS_SDK_FLOOR`], restamping it with `vtool` when it doesn't.
///
/// The macOS 26 interface redesign (larger traffic-light controls, Liquid
/// Glass window chrome) is gated by AppKit on the SDK the binary was *linked*
/// against — `dyld_program_sdk_at_least` reading this field — not on the OS it
/// runs on. Our Intel CI host maxes out at the macOS 13 SDK, so without this
/// its builds would ship the pre-redesign look even when run on Tahoe. `vtool`
/// rewrites the field in place (all slices of a universal binary at once);
/// `codesign` runs afterwards, so the mutation is inside the signed bundle.
/// The deployment target (`minos`) is kept at [`meta::MACOS_MIN_VERSION`].
///
/// A build *on* macOS 26 already has SDK ≥ the floor and is left untouched
/// (we never lower it).
fn ensure_min_sdk(binary: &Path) -> Result<(), String> {
    let floor = parse_version(meta::MACOS_SDK_FLOOR)
        .ok_or_else(|| format!("bad MACOS_SDK_FLOOR {:?}", meta::MACOS_SDK_FLOOR))?;

    let shown = Command::new("vtool")
        .arg("-show-build")
        .arg(binary)
        .output()
        .map_err(|e| format!("spawn vtool: {e}"))?;
    if !shown.status.success() {
        return Err(format!(
            "vtool -show-build failed: {}",
            String::from_utf8_lossy(&shown.stderr).trim()
        ));
    }
    let text = String::from_utf8_lossy(&shown.stdout);
    // Lowest `sdk` across all slices — if any is below the floor we restamp
    // (vtool rewrites every slice to the same value regardless).
    let current = text
        .lines()
        .filter_map(|l| l.trim().strip_prefix("sdk "))
        .filter_map(|v| parse_version(v.trim()))
        .min()
        .ok_or("vtool -show-build reported no LC_BUILD_VERSION sdk")?;

    if current >= floor {
        eprintln!(
            "dtb-ke-bundle: macOS SDK stamp {}.{} already ≥ {} — left as is",
            current.0,
            current.1,
            meta::MACOS_SDK_FLOOR
        );
        return Ok(());
    }

    let path = binary.to_string_lossy();
    let path = path.as_ref();
    try_run(
        "vtool",
        &[
            "-set-build-version",
            "macos",
            meta::MACOS_MIN_VERSION,
            meta::MACOS_SDK_FLOOR,
            "-replace",
            "-output",
            path,
            path,
        ],
        &workspace_root(),
    )?;
    eprintln!(
        "dtb-ke-bundle: restamped macOS SDK {}.{} → {} for the macOS 26 interface",
        current.0,
        current.1,
        meta::MACOS_SDK_FLOOR
    );
    Ok(())
}

/// `"26"` / `"26.0"` / `"26.1.2"` → `(major, minor)`; trailing components are
/// ignored (the SDK gate only cares about major.minor).
fn parse_version(s: &str) -> Option<(u32, u32)> {
    let mut parts = s.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next().unwrap_or("0").parse().ok()?;
    Some((major, minor))
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

#[cfg(test)]
mod tests {
    use super::info_plist;

    #[test]
    fn plist_declares_the_dtbke_document_type() {
        let xml = info_plist(Some("AppIcon"), Some("DocumentIcon"));
        assert_eq!(xml.matches('<').count(), xml.matches('>').count(), "unbalanced tags");
        assert!(xml.contains("<key>CFBundleDocumentTypes</key>"));
        assert!(xml.contains("<key>UTExportedTypeDeclarations</key>"));
        assert!(xml.contains("de.philippremy.dtb-kampfrichtereinsatzplan"));
        assert!(xml.contains("<string>dtbke</string>"));
        assert!(xml.contains("<key>UTTypeIconFile</key>"));
    }

    #[test]
    fn plist_omits_doc_icon_key_when_unavailable() {
        let xml = info_plist(None, None);
        assert_eq!(xml.matches('<').count(), xml.matches('>').count(), "unbalanced tags");
        assert!(!xml.contains("UTTypeIconFile"));
    }
}
