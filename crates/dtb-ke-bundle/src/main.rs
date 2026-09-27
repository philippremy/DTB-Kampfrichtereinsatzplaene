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
//!   cargo dtb-ke-bundle icons                      regenerate icon variants (macOS only,
//!                                                   see icon.rs; writes + commits to
//!                                                   assets/icons/generated/)
//!   cargo dtb-ke-bundle bundle  [--debug] [--formats a,b,c] [--sign <id>]
//!     iOS/iPadOS (macOS host only): `bundle --debug --target aarch64-apple-ios-sim` builds a
//!     simulator `.app`; `--target aarch64-apple-ios` a device one and additionally takes
//!     `--sign <identity> --provisioning-profile <file>`.
//!   Every build/bundle/icons/debug-info command takes `--product app|debugger` (default `app`): the debugger
//!   (`dtb-ke-debugger`) gets its own `.app`/`.msi`/Linux packages, icons (`assets/icons/debugger/`) and the
//!   `.dtbkedmp` file association, and has no crash helper and no iOS bundle.
//!   cargo dtb-ke-bundle debug-info [--universal | --target <t>] <out.tar.gz>
//!     package `[profile.release] split-debuginfo = "packed"`'s sidecar —
//!     macOS's `.dSYM`, Linux's `.dwp` — kept entirely separate from `bundle`
//!     so it never leaks into an installed `.deb`/`.rpm`/`.AppImage`. Windows
//!     (gnullvm) produces nothing to package here — see `debug_info.rs`.
//!   cargo dtb-ke-bundle symbols upload [--debug] [--strict] [--universal | --target <t>]
//!     PUT this build's debug file to the symbol server (`DTB_KE_SYMBOLS_URL` +
//!     `DTB_KE_SYMBOLS_UPLOAD_TOKEN`; best effort — see `symbols.rs`).

mod archive;
mod bundle;
mod codeberg;
mod debug_info;
mod helper;
mod icon;
mod ios;
mod linux;
mod macos;
mod manifest;
mod meta;
mod symbols;
mod util;
mod windows;

use std::process::exit;

use util::run;

fn main() {
    let mut argv = std::env::args().skip(1);
    let cmd = argv.next().unwrap_or_default();
    let rest: Vec<String> = argv.collect();
    let has = |flag: &str| rest.iter().any(|a| a == flag);
    match flag_value(&rest, "--product").as_deref() {
        None | Some("app") => meta::select(&meta::APP),
        Some("debugger") => meta::select(&meta::DEBUGGER),
        Some(other) => {
            eprintln!("dtb-ke-bundle: unknown --product {other:?} (expected `app` or `debugger`)");
            exit(2);
        }
    }

    match cmd.as_str() {
        "helper" => helper::stage(has("--release"), None),
        "build" => {
            let release = has("--release");
            if meta::p().crash_helper {
                helper::stage(release, None);
            }
            let mut args = vec!["build".to_string(), "-p".into(), meta::p().package.into()];
            if release {
                args.push("--release".into());
            }
            if let Some(pos) = rest.iter().position(|a| a == "--") {
                args.extend(rest[pos + 1..].iter().cloned());
            }
            run("cargo", &args);
        }
        "icons" => match icon::generate() {
            Ok(true) => eprintln!(
                "dtb-ke-bundle: icons written to {} — commit the result",
                icon::generated_dir().display()
            ),
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
            let provisioning_profile = flag_value(&rest, "--provisioning-profile");
            let mac_wrapper = has("--mac-wrapper");
            if let Err(e) = bundle::run(bundle::Options {
                release,
                formats: formats.map(parse_formats),
                sign,
                universal,
                target,
                provisioning_profile,
                mac_wrapper,
            }) {
                eprintln!("dtb-ke-bundle: {e}");
                exit(1);
            }
        }
        "debug-info" => {
            let release = !has("--debug");
            let universal = has("--universal");
            let target = flag_value(&rest, "--target");
            let Some(out) = rest.last().filter(|a| !a.starts_with("--")) else {
                eprintln!(
                    "dtb-ke-bundle: debug-info needs an output .tar.gz path as the last argument"
                );
                exit(2);
            };
            if let Err(e) = debug_info::run(debug_info::Options {
                release,
                universal,
                target,
                out: std::path::PathBuf::from(out),
            }) {
                eprintln!("dtb-ke-bundle: {e}");
                exit(1);
            }
        }
        "symbols" => {
            if rest.first().map(String::as_str) != Some("upload") {
                eprintln!(
                    "dtb-ke-bundle: usage: symbols upload [--debug] [--strict] [--universal | --target <t>]"
                );
                exit(2);
            }
            if let Err(e) = symbols::run(symbols::Options {
                release: !has("--debug"),
                universal: has("--universal"),
                target: flag_value(&rest, "--target"),
                strict: has("--strict"),
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
            // and is a real file on disk, is an asset to upload. Args that
            // don't resolve to a file (an unexpanded `*.deb` glob, a stray `\`
            // from an empty PowerShell splat, a directory) are skipped with a
            // warning rather than crashing later in `codeberg::upload` — but
            // an empty arg is dropped silently (it's just noise).
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
                .map(|(_, a)| (a, std::path::PathBuf::from(a)))
                .filter(|(raw, p)| {
                    if p.is_file() {
                        true
                    } else {
                        if !raw.is_empty() {
                            eprintln!("dtb-ke-bundle: skipping upload arg {raw:?} — not a file");
                        }
                        false
                    }
                })
                .map(|(_, p)| p)
                .collect();
            if has("--plain") {
                client.upload_plain(&release_ids, &files)
            } else {
                client.upload(&release_ids, &files)
            }
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
  (every command below also takes --product app|debugger; default app)
  cargo dtb-ke-bundle build  [--release]     stage the crash helper, then build the app
  cargo dtb-ke-bundle helper [--release]     just (re)stage the crash helper
  cargo dtb-ke-bundle icons                  regenerate .icns / .ico / PNG icons from the master
  cargo dtb-ke-bundle bundle [options]       build + package for the host OS
  cargo dtb-ke-bundle debug-info [options] <out.tar.gz>  package the split-debuginfo sidecar
  cargo dtb-ke-bundle manifest [options] <archive>...   write the self_update release manifest
  cargo dtb-ke-bundle codeberg <prepare|upload|manifest-publish> [options]   CI release helpers

codeberg subcommands (need $CODEBERG_TOKEN):
  prepare --tag <t> --target <commitish> --title <t> [--notes-file <p>] [--draft] [--prerelease]
      replace any existing release+tag named <t>, print/emit release_id=<id>
  upload [--plain] --release-id <id> [--release-id <id2> ...] <file>...
      upload each file (+ a small manifest fragment) to every given release —
      --plain skips the fragment, for assets self_update must never see
      (e.g. a debug-info archive: it would otherwise look like a downloadable
      app update to publish_manifest)
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

debug-info options (never errors when there's nothing to package — see RUNNERS.md):
  --debug              package the debug build's sidecar (default: release)
  --universal          macOS only: merge the two --universal slices' .dSYM with lipo
  --target <triple>    the specific target the sidecar was built for (default: host)
  <out.tar.gz>         required, must be the last argument
"
    );
}
