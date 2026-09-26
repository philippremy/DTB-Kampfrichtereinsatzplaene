//! iOS / iPadOS `.app` bundle (`--target aarch64-apple-ios-sim` or `aarch64-apple-ios`).
//!
//! Unlike macOS, an iOS bundle is flat:
//!
//! ```text
//! DTB Kampfrichtereinsatzpläne.app/
//!   Info.plist
//!   DTB-Kampfrichtereinsatzplaene      — the executable
//!   Assets.car                         — app icon (pre-compiled by `actool`, committed)
//!   embedded.mobileprovision           — device builds only
//! ```
//!
//! gpui's iOS backend needs no entry-point shim: `main()` runs UIKit's loop itself, so the
//! bundle is just the executable plus an `Info.plist` (the same recipe as the upstream
//! `gpui_ios` example's `build-simulator.sh`).

use std::path::Path;
use std::process::Command;

use crate::bundle::Context;
use crate::util::{copy, fresh_dir, report};
use crate::{icon, macos, meta};

/// Whether `target` names an iOS triple (`aarch64-apple-ios`, `aarch64-apple-ios-sim`, …).
pub fn is_ios_target(target: Option<&str>) -> bool {
    target.is_some_and(|t| t.contains("apple-ios"))
}

fn is_simulator(target: &str) -> bool {
    target.ends_with("-sim") || target.starts_with("x86_64")
}

pub fn bundle(cx: &Context) -> Result<(), String> {
    if !cfg!(target_os = "macos") {
        return Err("iOS bundles can only be built on macOS (Xcode tools are required)".into());
    }
    let target = cx
        .target
        .as_deref()
        .ok_or("iOS bundling needs --target <ios triple>")?;
    let simulator = is_simulator(target);
    if cx.mac_wrapper && simulator {
        return Err("--mac-wrapper needs a device build (--target aarch64-apple-ios)".into());
    }
    let (platform, sdk_name) = if simulator {
        ("iphonesimulator", "iphonesimulator")
    } else {
        ("iphoneos", "iphoneos")
    };

    let app = cx.out_dir.join(format!("{}.app", meta::p().display_name));
    fresh_dir(&app).map_err(io)?;

    let exe = app.join(meta::p().macos_executable_name);
    copy(&cx.binary, &exe).map_err(io)?;
    make_executable(&exe)?;
    // The legacy CI Mac links against an old SDK; iOS 26 features (Liquid Glass) are gated on the
    // *linked* SDK, so restamp it (before codesign, like the macOS path).
    macos::ensure_min_sdk(
        &exe,
        if simulator { "iossim" } else { "ios" },
        meta::IOS_MIN_VERSION,
        meta::IOS_SDK_FLOOR,
    )?;

    let document_icons = match write_document_icons(&app) {
        Ok(written) => written,
        Err(e) => {
            eprintln!("dtb-ke-bundle: warning — bundling without a document icon: {e}");
            false
        }
    };

    std::fs::write(
        app.join("Info.plist"),
        info_plist(platform, sdk_name, document_icons),
    )
    .map_err(io)?;

    if icon::ios_available() {
        if let Err(e) = install_icon(&app) {
            eprintln!("dtb-ke-bundle: warning — bundling without an app icon: {e}");
        }
    } else {
        eprintln!(
            "dtb-ke-bundle: warning — no pre-compiled iOS icon in {} (run `cargo dtb-ke-bundle icons` \
             on a Mac with Xcode 26 and commit the result)",
            icon::ios_dir().display()
        );
    }

    let profile = flag_env(&cx.provisioning_profile, "DTB_KE_PROVISIONING_PROFILE");
    let identity = flag_env(&cx.sign, "DTB_KE_SIGN_ID");
    let entitlements = match (&profile, simulator) {
        (Some(profile), false) => {
            copy(Path::new(profile), &app.join("embedded.mobileprovision")).map_err(io)?;
            Some(entitlements_from_profile(profile, &cx.out_dir)?)
        }
        _ => None,
    };
    if !simulator && (profile.is_none() || identity.is_none()) {
        eprintln!(
            "dtb-ke-bundle: warning — a device build needs --sign <identity> and \
             --provisioning-profile <file> (or DTB_KE_SIGN_ID / DTB_KE_PROVISIONING_PROFILE); \
             signing ad-hoc, which iOS will refuse to install"
        );
    }
    codesign(
        &app,
        identity.as_deref().unwrap_or("-"),
        entitlements.as_deref(),
    )?;

    report(&app);
    if !simulator {
        let ipa = write_ipa(&app, &cx.out_dir)?;
        report(&ipa);
    }
    if cx.mac_wrapper {
        let wrapped = wrap_for_mac(&app, &cx.out_dir.join("mac-wrapper"))?;
        report(&wrapped);
        eprintln!(
            "dtb-ke-bundle: Mac wrapper (\"Designed for iPad\") written — it only launches on a Mac when \
             the embedded profile allows Apple Silicon Macs; run it with `open \"{}\"`",
            wrapped.display()
        );
    }
    if simulator {
        eprintln!(
            "dtb-ke-bundle: install with `xcrun simctl install booted \"{}\"` and launch with \
             `xcrun simctl launch booted {}`",
            app.display(),
            meta::p().rdns_id
        );
    }
    Ok(())
}

/// The flag, else the env var — either counts only when non-empty (CI passes unset secrets as "").
fn flag_env(flag: &Option<String>, var: &str) -> Option<String> {
    flag.clone()
        .or_else(|| std::env::var(var).ok())
        .filter(|v| !v.trim().is_empty())
}

fn info_plist(platform: &str, sdk_name: &str, document_icons: bool) -> String {
    // The bundle id must be ASCII on iOS (`meta::p().identifier` carries an "ä"), hence RDNS_ID.
    // iPad only (UIDeviceFamily 2); multitasking is allowed (no UIRequiresFullScreen).
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
	<key>CFBundleDevelopmentRegion</key>
	<string>de</string>
	<key>CFBundleExecutable</key>
	<string>{exe}</string>
	<key>CFBundleIdentifier</key>
	<string>{id}</string>
	<key>CFBundleInfoDictionaryVersion</key>
	<string>6.0</string>
	<key>CFBundleName</key>
	<string>{name}</string>
	<key>CFBundleDisplayName</key>
	<string>{display_name}</string>
	<key>CFBundlePackageType</key>
	<string>APPL</string>
	<key>CFBundleShortVersionString</key>
	<string>{version}</string>
	<key>CFBundleVersion</key>
	<string>{version}</string>
	<key>CFBundleSupportedPlatforms</key>
	<array>
		<string>{sdk}</string>
	</array>
	<key>DTPlatformName</key>
	<string>{platform}</string>
	<key>MinimumOSVersion</key>
	<string>{min}</string>
	<key>UIDeviceFamily</key>
	<array>
		<integer>2</integer>
	</array>
	<key>LSRequiresIPhoneOS</key>
	<true/>
	<key>UIRequiredDeviceCapabilities</key>
	<array>
		<string>arm64</string>
	</array>
	<key>UIApplicationSceneManifest</key>
	<dict>
		<key>UIApplicationSupportsMultipleScenes</key>
		<false/>
		<key>UISceneConfigurations</key>
		<dict>
			<key>UIWindowSceneSessionRoleApplication</key>
			<array>
				<dict>
					<key>UISceneConfigurationName</key>
					<string>Default Configuration</string>
				</dict>
			</array>
		</dict>
	</dict>
	<key>UILaunchScreen</key>
	<dict/>
	<key>UISupportedInterfaceOrientations~ipad</key>
	<array>
		<string>UIInterfaceOrientationPortrait</string>
		<string>UIInterfaceOrientationPortraitUpsideDown</string>
		<string>UIInterfaceOrientationLandscapeLeft</string>
		<string>UIInterfaceOrientationLandscapeRight</string>
	</array>
	<key>LSApplicationCategoryType</key>
	<string>{category}</string>
	<key>LSSupportsOpeningDocumentsInPlace</key>
	<true/>
	<key>CFBundleDocumentTypes</key>
	<array>
		<dict>
{document_icon_files}			<key>CFBundleTypeName</key>
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
{ut_icon_files}			<key>UTTypeIdentifier</key>
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
					<string>{doc_ext}</string>
				</array>
			</dict>
		</dict>
	</array>
</dict>
</plist>
"#,
        exe = meta::p().macos_executable_name,
        id = meta::p().rdns_id,
        name = meta::p().display_name,
        display_name = meta::IOS_DISPLAY_NAME,
        version = meta::numeric_version(),
        sdk = sdk_name,
        platform = platform,
        min = meta::IOS_MIN_VERSION,
        category = meta::p().macos_category,
        doc_name = meta::p().doc_type_name,
        uti = meta::p().doc_uti,
        doc_ext = meta::p().doc_extension,
        document_icon_files = icon_files_entry("CFBundleTypeIconFiles", document_icons),
        ut_icon_files = icon_files_entry("UTTypeIconFiles", document_icons),
    )
}

/// Install the pre-compiled iOS app icon (`assets/icons/generated/ios/`, see `icon::ios_dir`): copy
/// `Assets.car` and the fallback PNGs into the app and merge the icon keys `actool` reported
/// (`CFBundleIcons~ipad`, `CFBundleIconName`) into `Info.plist`. Compiled ahead of time by
/// `cargo dtb-ke-bundle icons` because it needs Xcode 26's `actool`, which CI hosts may lack.
fn install_icon(app: &Path) -> Result<(), String> {
    for entry in std::fs::read_dir(icon::ios_dir()).map_err(io)? {
        let path = entry.map_err(io)?.path();
        if path.is_file() && path != icon::ios_icon_plist() {
            copy(&path, &app.join(path.file_name().unwrap())).map_err(io)?;
        }
    }
    let status = Command::new("/usr/libexec/PlistBuddy")
        .arg("-c")
        .arg(format!("Merge \"{}\"", icon::ios_icon_plist().display()))
        .arg(app.join("Info.plist"))
        .status()
        .map_err(|e| format!("failed to spawn PlistBuddy: {e}"))?;
    if !status.success() {
        return Err("merging the icon keys into Info.plist failed".into());
    }
    Ok(())
}

/// Sizes of the `.dtbke` document icon shipped in the bundle (points; each also as `@2x`), the two
/// Apple recommends for document types.
const DOCUMENT_ICON_POINTS: [u32; 2] = [64, 320];

fn document_icon_names() -> Vec<String> {
    DOCUMENT_ICON_POINTS
        .iter()
        .map(|points| format!("DocumentIcon-{points}.png"))
        .collect()
}

/// Write the document icon (`assets/icons/FileIcon.png`) into the bundle as square, transparent
/// PNGs at each size and scale, named for `CFBundleTypeIconFiles` / `UTTypeIconFiles`. `Ok(false)`
/// when there is no master yet (the system then shows its generic document icon).
fn write_document_icons(app: &Path) -> Result<bool, String> {
    let master = icon::doc_master_path();
    if !master.exists() {
        return Ok(false);
    }
    let source = image::open(&master).map_err(|e| format!("reading {}: {e}", master.display()))?;
    for points in DOCUMENT_ICON_POINTS {
        for (scale, suffix) in [(1, ""), (2, "@2x")] {
            let side = points * scale;
            let canvas = fit_in_square(&source, side);
            let path = app.join(format!("DocumentIcon-{points}{suffix}.png"));
            canvas
                .save_with_format(&path, image::ImageFormat::Png)
                .map_err(|e| format!("writing {}: {e}", path.display()))?;
        }
    }
    Ok(true)
}

/// `source` scaled to fit a `side`² transparent canvas (4 % margin), centred, aspect preserved.
fn fit_in_square(source: &image::DynamicImage, side: u32) -> image::RgbaImage {
    let inner = (side as f32 * 0.92).round().max(1.0);
    let scale = inner / source.width().max(source.height()) as f32;
    let (width, height) = (
        (source.width() as f32 * scale).round().max(1.0) as u32,
        (source.height() as f32 * scale).round().max(1.0) as u32,
    );
    let resized = source
        .resize_exact(width, height, image::imageops::FilterType::Lanczos3)
        .to_rgba8();
    let mut canvas = image::RgbaImage::new(side, side);
    image::imageops::overlay(
        &mut canvas,
        &resized,
        i64::from((side - width) / 2),
        i64::from((side - height) / 2),
    );
    canvas
}

/// A plist `<key>` + array of the document icon file names (empty when there are none).
fn icon_files_entry(key: &str, present: bool) -> String {
    if !present {
        return String::new();
    }
    let names: String = document_icon_names()
        .iter()
        .map(|name| format!("\t\t\t\t<string>{name}</string>\n"))
        .collect();
    format!("\t\t\t<key>{key}</key>\n\t\t\t<array>\n{names}\t\t\t</array>\n")
}

/// Extract the `Entitlements` dict of a provisioning profile into a plist file.
fn entitlements_from_profile(profile: &str, out_dir: &Path) -> Result<std::path::PathBuf, String> {
    let decoded = Command::new("security")
        .args(["cms", "-D", "-i", profile])
        .output()
        .map_err(|e| format!("failed to spawn security: {e}"))?;
    if !decoded.status.success() {
        return Err(format!("could not decode {profile} with `security cms`"));
    }
    let profile_plist = out_dir.join("provisioning-profile.plist");
    std::fs::write(&profile_plist, &decoded.stdout).map_err(io)?;
    let entitlements = out_dir.join("entitlements.plist");
    let status = Command::new("plutil")
        .args(["-extract", "Entitlements", "xml1", "-o"])
        .arg(&entitlements)
        .arg(&profile_plist)
        .status()
        .map_err(|e| format!("failed to spawn plutil: {e}"))?;
    std::fs::remove_file(&profile_plist).ok();
    if !status.success() {
        return Err(format!("{profile} has no Entitlements dict"));
    }
    Ok(entitlements)
}

fn codesign(app: &Path, identity: &str, entitlements: Option<&Path>) -> Result<(), String> {
    let mut cmd = Command::new("codesign");
    cmd.args(["--force", "--sign", identity, "--timestamp=none"]);
    if let Some(entitlements) = entitlements {
        cmd.arg("--entitlements").arg(entitlements);
    }
    let status = cmd
        .arg(app)
        .status()
        .map_err(|e| format!("failed to spawn codesign: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("codesign exited with {status}"))
    }
}

/// Copy a finished (already signed) iOS `.app` into the layout macOS launches iOS apps from:
/// `<dest>/<Name>.app/{Wrapper/<Name>.app, WrappedBundle -> Wrapper/<Name>.app}`. Returns the outer
/// bundle. `ditto` copies so the code signature survives; the input `.app` is left untouched.
fn wrap_for_mac(app: &Path, dest: &Path) -> Result<std::path::PathBuf, String> {
    let name = app.file_name().ok_or("bad app path")?.to_owned();
    fresh_dir(dest).map_err(io)?;
    let outer = dest.join(&name);
    let wrapper = outer.join("Wrapper");
    std::fs::create_dir_all(&wrapper).map_err(io)?;
    run_tool("ditto", &[app.as_os_str(), wrapper.join(&name).as_os_str()])?;
    let link = format!("Wrapper/{}", name.to_string_lossy());
    symlink(link, &outer.join("WrappedBundle"))?;
    Ok(outer)
}

/// `<DISPLAY_NAME>.ipa` next to the `.app`: a zip with the bundle under `Payload/`.
fn write_ipa(app: &Path, out_dir: &Path) -> Result<std::path::PathBuf, String> {
    let stage = out_dir.join("ipa-stage");
    fresh_dir(&stage).map_err(io)?;
    let payload = stage.join("Payload");
    std::fs::create_dir_all(&payload).map_err(io)?;
    let name = app.file_name().ok_or("bad app path")?;
    run_tool("ditto", &[app.as_os_str(), payload.join(name).as_os_str()])?;
    let ipa = out_dir.join(format!("{}.ipa", meta::p().display_name));
    std::fs::remove_file(&ipa).ok();
    let status = Command::new("zip")
        .current_dir(&stage)
        .args(["-qry"])
        .arg(&ipa)
        .arg("Payload")
        .status()
        .map_err(|e| format!("failed to spawn zip: {e}"))?;
    if !status.success() {
        return Err(format!("zip exited with {status}"));
    }
    std::fs::remove_dir_all(&stage).ok();
    Ok(ipa)
}

fn run_tool(tool: &str, args: &[&std::ffi::OsStr]) -> Result<(), String> {
    let status = Command::new(tool)
        .args(args)
        .status()
        .map_err(|e| format!("failed to spawn {tool}: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("{tool} exited with {status}"))
    }
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(io)
}

// iOS bundles are only built on macOS (`bundle` bails first); this only keeps other hosts compiling.
#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<(), String> {
    Err("iOS bundles can only be built on macOS".into())
}

#[cfg(unix)]
fn symlink(target: String, link: &Path) -> Result<(), String> {
    std::os::unix::fs::symlink(target, link).map_err(io)
}

#[cfg(not(unix))]
fn symlink(_target: String, _link: &Path) -> Result<(), String> {
    Err("iOS bundles can only be built on macOS".into())
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}
