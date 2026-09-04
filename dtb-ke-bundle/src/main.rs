//! `cargo dtb-ke-bundle <command>` — build orchestration + platform packaging.
//!
//! `dtb-ke-crash` embeds the crash helper with `include_bytes!`, but cargo won't
//! build a sibling `[[bin]]` unless something depends on it (and artifact deps
//! need `-Z bindeps`). So: build the helper first, stage it where the
//! `dtb-ke-crash` build script reads it, then build the app.
//!
//! On top of that, `bundle` turns the built binary into a distributable package
//! for the host OS — a `.app` on macOS, an `.msi` (via WiX) on Windows, and
//! `.deb` / `.rpm` / `.AppImage` / `.tar.gz` on Linux.
//!
//!   cargo dtb-ke-bundle build   [--release] [-- <extra cargo args>]
//!   cargo dtb-ke-bundle helper  [--release]        just (re)stage the helper
//!   cargo dtb-ke-bundle icons                      regenerate icon variants
//!   cargo dtb-ke-bundle bundle  [--debug] [--formats a,b,c] [--sign <id>]

mod archive;
mod bundle;
mod codeberg;
mod helper;
mod icon;
mod linux;
mod macos;
mod manifest;
mod meta;
mod util;
mod windows;

use std::process::exit;

use util::run;

fn main() {
    let mut argv = std::env::args().skip(1);
    let cmd = argv.next().unwrap_or_default();
    let rest: Vec<String> = argv.collect();
    let has = |flag: &str| rest.iter().any(|a| a == flag);

    match cmd.as_str() {
        "helper" => helper::stage(has("--release"), None),
        "build" => {
            let release = has("--release");
            helper::stage(release, None);
            let mut args = vec!["build".to_string(), "-p".into(), "dtb-ke-ui".into()];
            if release {
                args.push("--release".into());
            }
            if let Some(pos) = rest.iter().position(|a| a == "--") {
                args.extend(rest[pos + 1..].iter().cloned());
            }
            run("cargo", &args);
        }
        "icons" => match icon::generate() {
            Ok(true) => eprintln!("dtb-ke-bundle: icons written to target/bundle/icon/"),
            Ok(false) => exit(1),
            Err(e) => {
                eprintln!("dtb-ke-bundle: {e}");
                exit(1);
            }
        },
        "bundle" => {
            // Packaging ships the optimised binary unless told otherwise.
            let release = !has("--debug");
            let formats = flag_value(&rest, "--formats");
            let sign = flag_value(&rest, "--sign");
            let universal = has("--universal");
            let target = flag_value(&rest, "--target");
            if let Err(e) = bundle::run(bundle::Options {
                release,
                formats: formats.map(parse_formats),
                sign,
                universal,
                target,
            }) {
                eprintln!("dtb-ke-bundle: {e}");
                exit(1);
            }
        }
        "manifest" => {
            let archives: Vec<std::path::PathBuf> = rest
                .iter()
                .filter(|a| a.ends_with(".tar.gz") || a.ends_with(".tgz") || a.ends_with(".zip"))
                .map(std::path::PathBuf::from)
                .collect();
            let Some(version) = flag_value(&rest, "--version") else {
                eprintln!("dtb-ke-bundle: manifest needs --version <x.y.z>");
                exit(2);
            };
            if let Err(e) = manifest::run(manifest::Options {
                version,
                date: flag_value(&rest, "--date"),
                notes_url: flag_value(&rest, "--notes-url"),
                base_url: flag_value(&rest, "--base-url"),
                out: flag_value(&rest, "--out")
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| "manifest.json".into()),
                archives,
            }) {
                eprintln!("dtb-ke-bundle: {e}");
                exit(1);
            }
        }
        "codeberg" => run_codeberg(&rest),
        "" | "-h" | "--help" | "help" => usage(),
        other => {
            eprintln!("dtb-ke-bundle: unknown command {other:?}\n");
            usage();
            exit(2);
        }
    }
}

/// `cargo dtb-ke-bundle codeberg <prepare|upload|manifest-publish> [options]`.
fn run_codeberg(rest: &[String]) {
    let sub = rest.first().cloned().unwrap_or_default();
    let rest = &rest[rest.len().min(1)..];
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    let client = codeberg::Client::from_env().unwrap_or_else(|e| {
        eprintln!("dtb-ke-bundle: {e}");
        exit(2);
    });

    let result = match sub.as_str() {
        "prepare" => {
            let (Some(tag), Some(target), Some(title)) = (
                flag_value(rest, "--tag"),
                flag_value(rest, "--target"),
                flag_value(rest, "--title"),
            ) else {
                eprintln!("dtb-ke-bundle: codeberg prepare needs --tag, --target, --title");
                exit(2);
            };
            let notes =
                flag_value(rest, "--notes-file").and_then(|p| std::fs::read_to_string(p).ok());
            client
                .prepare(codeberg::PrepareOptions {
                    tag: &tag,
                    target: &target,
                    title: &title,
                    notes,
                    draft: has("--draft"),
                    prerelease: has("--prerelease"),
                })
                .map(|_| ())
        }
        "upload" => {
            // --release-id may repeat (uploads to every one, e.g. a version
            // tag's release *and* the rolling `latest` release).
            let release_ids: Vec<u64> = rest
                .iter()
                .zip(rest.iter().skip(1))
                .filter(|(flag, _)| *flag == "--release-id")
                .filter_map(|(_, v)| v.parse().ok())
                .collect();
            if release_ids.is_empty() {
                eprintln!("dtb-ke-bundle: codeberg upload needs at least one --release-id <id>");
                exit(2);
            }
            // Everything that isn't `--release-id` or a value right after it,
            // and exists on disk, is a file to upload.
            let value_indices: std::collections::HashSet<usize> = rest
                .iter()
                .enumerate()
                .filter(|(_, a)| *a == "--release-id")
                .map(|(i, _)| i + 1)
                .collect();
            let files: Vec<std::path::PathBuf> = rest
                .iter()
                .enumerate()
                .filter(|(i, a)| !value_indices.contains(i) && !a.starts_with("--"))
                .map(|(_, a)| std::path::PathBuf::from(a))
                .filter(|p| p.exists())
                .collect();
            client.upload(&release_ids, &files)
        }
        "manifest-publish" => {
            let (Some(release_id), Some(version)) = (
                flag_value(rest, "--release-id").and_then(|v| v.parse().ok()),
                flag_value(rest, "--version"),
            ) else {
                eprintln!(
                    "dtb-ke-bundle: codeberg manifest-publish needs --release-id and --version"
                );
                exit(2);
            };
            client.publish_manifest(
                release_id,
                codeberg::ManifestOptions {
                    version,
                    date: flag_value(rest, "--date"),
                    notes_url: flag_value(rest, "--notes-url"),
                    publish: has("--publish"),
                },
            )
        }
        other => {
            eprintln!("dtb-ke-bundle: unknown codeberg subcommand {other:?}\n");
            usage();
            exit(2);
        }
    };
    if let Err(e) = result {
        eprintln!("dtb-ke-bundle: {e}");
        exit(1);
    }
}

/// `--formats deb,rpm` → `["deb", "rpm"]`.
fn parse_formats(raw: String) -> Vec<String> {
    raw.split(',')
        .map(|s| s.trim().to_ascii_lowercase())
        .filter(|s| !s.is_empty())
        .collect()
}

/// The value after `--flag` (`--flag x`), or `None`.
fn flag_value(rest: &[String], flag: &str) -> Option<String> {
    rest.iter()
        .position(|a| a == flag)
        .and_then(|i| rest.get(i + 1))
        .cloned()
}

fn usage() {
    eprintln!(
        "usage:
  cargo dtb-ke-bundle build  [--release]     stage the crash helper, then build the app
  cargo dtb-ke-bundle helper [--release]     just (re)stage the crash helper
  cargo dtb-ke-bundle icons                  regenerate .icns / .ico / PNG icons from the master
  cargo dtb-ke-bundle bundle [options]       build + package for the host OS
  cargo dtb-ke-bundle manifest [options] <archive>...   write the self_update release manifest
  cargo dtb-ke-bundle codeberg <prepare|upload|manifest-publish> [options]   CI release helpers

codeberg subcommands (need $CODEBERG_TOKEN):
  prepare --tag <t> --target <commitish> --title <t> [--notes-file <p>] [--draft] [--prerelease]
      replace any existing release+tag named <t>, print/emit release_id=<id>
  upload --release-id <id> [--release-id <id2> ...] <file>...
      upload each file (+ a small manifest fragment) to every given release
  manifest-publish --release-id <id> --version <v> [--date <d>] [--notes-url <u>] [--publish]
      merge every fragment on the release into manifest.json, upload it, clean up

manifest options:
  --version <x.y.z>    the release version (required)
  --date <YYYY-MM-DD>  release date
  --notes-url <url>    changelog / release-page URL
  --base-url <url>     prefix for asset URLs (default: relative to the manifest)
  --out <path>         output file (default: manifest.json)

bundle options:
  --debug              package the debug binary (default: release)
  --formats a,b,c      restrict Linux output (deb, rpm, appimage, tar); default: all available
  --sign <identity>    codesign identity for the macOS .app (default: ad-hoc \"-\")
  --universal          macOS only: build x86_64 + aarch64 and lipo them into one binary
  --target <triple>    cross/slice-build for this target instead of the host's native one
"
    );
}
