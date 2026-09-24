//! iOS / iPadOS `.app` bundle (`--target aarch64-apple-ios-sim` or `aarch64-apple-ios`).
//!
//! Unlike macOS, an iOS bundle is flat:
//!
//! ```text
//! DTB Kampfrichtereinsatzpläne.app/
//!   Info.plist
//!   DTB-Kampfrichtereinsatzplaene      — the executable
//!   Assets.car                         — app icon (compiled by `actool`)
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
use crate::{icon, meta};

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
    let target = cx.target.as_deref().ok_or("iOS bundling needs --target <ios triple>")?;
    let simulator = is_simulator(target);
    let (platform, sdk_name) = if simulator {
        ("iphonesimulator", "iphonesimulator")
    } else {
        ("iphoneos", "iphoneos")
    };

    let app = cx.out_dir.join(format!("{}.app", meta::DISPLAY_NAME));
    fresh_dir(&app).map_err(io)?;

    let exe = app.join(meta::MACOS_EXECUTABLE_NAME);
    copy(&cx.binary, &exe).map_err(io)?;
    make_executable(&exe)?;

    std::fs::write(app.join("Info.plist"), info_plist(platform, sdk_name)).map_err(io)?;

    if cx.have_icon {
        if let Err(e) = compile_icon(&app, platform) {
            eprintln!("dtb-ke-bundle: warning — bundling without an app icon: {e}");
        }
    }

    let profile = flag_env(&cx.provisioning_profile, "DTB_KE_PROVISIONING_PROFILE");
    let identity = cx
        .sign
        .clone()
        .or_else(|| std::env::var("DTB_KE_SIGN_ID").ok());
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
    codesign(&app, identity.as_deref().unwrap_or("-"), entitlements.as_deref())?;

    report(&app);
    if simulator {
        eprintln!(
            "dtb-ke-bundle: install with `xcrun simctl install booted \"{}\"` and launch with \
             `xcrun simctl launch booted {}`",
            app.display(),
            meta::RDNS_ID
        );
    }
    Ok(())
}

fn flag_env(flag: &Option<String>, var: &str) -> Option<String> {
    flag.clone().or_else(|| std::env::var(var).ok())
}

fn info_plist(platform: &str, sdk_name: &str) -> String {
    // The bundle id must be ASCII on iOS (`meta::IDENTIFIER` carries an "ä"), hence RDNS_ID.
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
	<string>{name}</string>
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
	<key>CFBundleDocumentTypes</key>
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
					<string>{doc_ext}</string>
				</array>
			</dict>
		</dict>
	</array>
</dict>
</plist>
"#,
        exe = meta::MACOS_EXECUTABLE_NAME,
        id = meta::RDNS_ID,
        name = meta::DISPLAY_NAME,
        version = meta::numeric_version(),
        sdk = sdk_name,
        platform = platform,
        min = meta::IOS_MIN_VERSION,
        category = meta::MACOS_CATEGORY,
        doc_name = meta::DOC_TYPE_NAME,
        uti = meta::DOC_UTI,
        doc_ext = meta::DOC_EXTENSION,
    )
}

/// Compile the flat 1024² master into `Assets.car` with `actool` and merge the icon keys it
/// reports (`CFBundleIcons~ipad`) into `Info.plist`.
fn compile_icon(app: &Path, platform: &str) -> Result<(), String> {
    let master = icon::flat_png_path();
    if !master.exists() {
        return Err(format!("{} not found", master.display()));
    }
    let work = app.parent().unwrap().join("ios-icon-work");
    fresh_dir(&work).map_err(io)?;
    let set = work.join("Assets.xcassets/AppIcon.appiconset");
    copy(&master, &set.join("icon-1024.png")).map_err(io)?;
    std::fs::write(
        work.join("Assets.xcassets/Contents.json"),
        r#"{ "info" : { "author" : "xcode", "version" : 1 } }"#,
    )
    .map_err(io)?;
    std::fs::write(
        set.join("Contents.json"),
        r#"{
  "images" : [ { "filename" : "icon-1024.png", "idiom" : "universal", "platform" : "ios", "size" : "1024x1024" } ],
  "info" : { "author" : "xcode", "version" : 1 }
}"#,
    )
    .map_err(io)?;

    let partial = work.join("partial.plist");
    let out = Command::new("xcrun")
        .args(["actool", "--platform", platform, "--minimum-deployment-target", meta::IOS_MIN_VERSION])
        .args(["--target-device", "ipad", "--app-icon", icon::APP_ICON_NAME])
        .arg("--output-partial-info-plist")
        .arg(&partial)
        .arg("--compile")
        .arg(app)
        .arg(work.join("Assets.xcassets"))
        .output()
        .map_err(|e| format!("failed to spawn actool: {e}"))?;
    if !out.status.success() {
        return Err(format!("actool failed: {}", String::from_utf8_lossy(&out.stdout)));
    }
    if partial.exists() {
        let status = Command::new("/usr/libexec/PlistBuddy")
            .arg("-c")
            .arg(format!("Merge \"{}\"", partial.display()))
            .arg(app.join("Info.plist"))
            .status()
            .map_err(|e| format!("failed to spawn PlistBuddy: {e}"))?;
        if !status.success() {
            return Err("merging the actool icon keys into Info.plist failed".into());
        }
    }
    std::fs::remove_dir_all(&work).ok();
    Ok(())
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

fn make_executable(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).map_err(io)
}

fn io(e: std::io::Error) -> String {
    e.to_string()
}
