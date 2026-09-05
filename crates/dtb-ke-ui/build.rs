//! Collects build / VCS / toolchain metadata and every dependency's licence
//! text, and writes them to `$OUT_DIR/build_meta.rs` for `src/build_info.rs` to
//! `include!`.
//!
//! Every step is best-effort: anything that can't be determined becomes `None`
//! (rendered as "nicht verfügbar" in the About window). The build never fails
//! because of missing metadata.
#![allow(clippy::collapsible_if)]

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::{env, fs};

use rayon::prelude::*;

fn main() {
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR"));
    let manifest_dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest_dir
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
        .unwrap_or_else(|| manifest_dir.clone());

    // Re-run when the lockfile or the checked-out commit changes.
    rerun("Cargo.lock");
    rerun("build.rs");
    // `build_info::APP_VERSION` reads this (CI Tip-build version override);
    // cargo tracks `option_env!` on its own, but the Tip runners reuse a
    // persistent target dir, so be explicit.
    println!("cargo:rerun-if-env-changed=DTB_KE_VERSION");
    for p in [".git/HEAD", ".git/index"] {
        let g = workspace_root.join(p);
        if g.exists() {
            println!("cargo:rerun-if-changed={}", g.display());
        }
    }

    let mut meta = String::new();

    // ── scalar metadata ────────────────────────────────────────────────────
    let profile = env::var("PROFILE").unwrap_or_default();
    let prof = ProfileSettings::resolve(&workspace_root, &profile);

    let (rustc_release, llvm) = rustc_version();

    put_str(&mut meta, "PROFILE", &profile);
    put_str(
        &mut meta,
        "OPT_LEVEL",
        &env::var("OPT_LEVEL").unwrap_or_default(),
    );
    put_str(
        &mut meta,
        "TARGET_TRIPLE",
        &env::var("TARGET").unwrap_or_default(),
    );
    put_str(
        &mut meta,
        "HOST_TRIPLE",
        &env::var("HOST").unwrap_or_default(),
    );
    put_opt(&mut meta, "DEBUG_INFO", prof.debug);
    put_opt(&mut meta, "LTO", prof.lto);
    put_opt(&mut meta, "CODEGEN_UNITS", prof.codegen_units);
    put_opt(&mut meta, "STRIP", prof.strip);
    put_opt(&mut meta, "INCREMENTAL", prof.incremental);
    put_opt(&mut meta, "RUST_VERSION", rustc_release);
    put_opt(&mut meta, "LLVM_VERSION", llvm);
    put_opt(&mut meta, "LINKER", linker());

    // ── VCS ────────────────────────────────────────────────────────────────
    let vcs = Vcs::detect(&workspace_root);
    put_opt(&mut meta, "COMMIT", vcs.commit);
    put_opt(&mut meta, "BRANCH", vcs.branch);
    put_opt(&mut meta, "COMMIT_DATE", vcs.commit_date);
    put_opt(&mut meta, "WORKING_TREE", vcs.working_tree);

    // ── dependencies + licences ────────────────────────────────────────────
    let lock = fs::read_to_string(workspace_root.join("Cargo.lock")).unwrap_or_default();
    let packages = parse_lock(&lock);
    let workspace_members: Vec<&str> = [
        "dtb-ke-ui",
        "dtb-ke-export",
        "dtb-ke-log",
        "dtb-ke-persist",
        "dtb-ke-resource",
        "dtb-ke-types",
        "dtb-ke-util",
    ]
    .to_vec();
    let deps: Vec<&Package> = packages
        .iter()
        .filter(|p| !workspace_members.contains(&p.name.as_str()))
        .collect();

    meta.push_str(&format!(
        "pub const DEPENDENCY_COUNT: usize = {};\n",
        deps.len()
    ));

    // Resolving every dependency to its unpacked source dir, then reading its
    // SPDX id and licence text, is almost the entire cost of this build script
    // — a filesystem-heavy crawl of `~/.cargo`. Each dependency's lookup is
    // independent, read-only work, so do them in parallel; the crate-source
    // index is built once up front so a lookup is a hashmap hit rather than a
    // fresh directory walk per dependency.
    let index = SourceIndex::build(&deps);
    let resolved: Vec<(Option<String>, Option<String>)> = deps
        .par_iter()
        .map(|p| {
            let dir = index.locate(p);
            let spdx = dir.as_deref().and_then(read_spdx);
            let license = dir.as_deref().and_then(read_license_text);
            (spdx, license)
        })
        .collect();

    // Fold the results back together sequentially, in `deps` order, so the
    // `LICENSE_TEXTS` indices — and the whole generated file — stay
    // deterministic regardless of how the work was scheduled.
    let mut texts: Vec<String> = Vec::new();
    let mut text_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut rows = String::new();
    for (p, (spdx, license)) in deps.iter().zip(resolved) {
        let license_ref = license.map(|t| {
            *text_index.entry(t.clone()).or_insert_with(|| {
                texts.push(t);
                texts.len() - 1
            })
        });
        rows.push_str(&format!(
            "    D {{ name: {:?}, version: {:?}, spdx: {}, license: {} }},\n",
            p.name,
            p.version,
            opt_lit(spdx.as_deref()),
            match license_ref {
                Some(i) => format!("Some({i})"),
                None => "None".to_string(),
            }
        ));
    }

    meta.push_str("pub static LICENSE_TEXTS: &[&str] = &[\n");
    for t in &texts {
        meta.push_str(&format!("    {t:?},\n"));
    }
    meta.push_str("];\n");
    meta.push_str("pub static DEPENDENCIES: &[D] = &[\n");
    meta.push_str(&rows);
    meta.push_str("];\n");

    fs::write(out.join("build_meta.rs"), meta).expect("write build_meta.rs");

    emit_smtp_secret(&out);
    emit_update_key(&out, &workspace_root);
    embed_windows_resource(&out, &workspace_root);
}

// ── in-app updater: the embedded artifact-verification key ────────────────
//
// `assets/release.pub` is the **public** half of the zipsign / ed25519 key pair
// whose private half signs release archives in CI. It's public — no obfuscation
// — but the app must ship *some* trusted key or the updater has nothing to
// verify against, so an absent / wrong-length file emits `None` and
// `updater::available()` is `false` (the whole feature turns off), exactly like
// `mail::available()` with no SMTP credentials.
//
// `DTB_KE_RELEASE_PUB` overrides the path — the local testbed
// (`scripts/updater-testbed.sh`) points it at a throwaway keypair so local
// updater testing never has to touch the committed `assets/release.pub`.
fn emit_update_key(out: &Path, workspace_root: &Path) {
    println!("cargo:rerun-if-env-changed=DTB_KE_RELEASE_PUB");
    let path = env::var_os("DTB_KE_RELEASE_PUB")
        .map(PathBuf::from)
        .unwrap_or_else(|| workspace_root.join("assets/release.pub"));
    println!("cargo:rerun-if-changed={}", path.display());

    let key = match fs::read(&path) {
        Ok(bytes) if bytes.len() == 32 => Some(bytes),
        Ok(bytes) => {
            println!(
                "cargo:warning=dtb-ke-ui: {} is {} bytes, expected 32 (raw ed25519 public key) — \
                 the in-app updater is disabled in this build",
                path.display(),
                bytes.len()
            );
            None
        }
        Err(_) => {
            println!(
                "cargo:warning=dtb-ke-ui: {} not found — the in-app updater is disabled in this \
                 build",
                path.display()
            );
            None
        }
    };

    let body = match key {
        Some(bytes) => format!(
            "// @generated by build.rs from {}\n\
             pub const VERIFY_KEY: Option<[u8; 32]> = Some({bytes:?});\n",
            path.display()
        ),
        None => format!(
            "// @generated by build.rs — no key at {}\n\
             pub const VERIFY_KEY: Option<[u8; 32]> = None;\n",
            path.display()
        ),
    };
    fs::write(out.join("update_key.rs"), body).expect("write update_key.rs");
}

// ── Windows executable resource (.ico + VERSIONINFO) ──────────────────────
//
// A no-op unless `TARGET` is a Windows triple. Rasterises the shared master
// `assets/icons/AppIcon.png` down to a multi-resolution `app.ico`, writes a
// tiny `app.rc` next to it, and hands that to `embed-resource`, which selects
// the resource compiler per target — `rc.exe` (MSVC), `llvm-rc` (building on
// Windows for gnu/gnullvm), or `<arch>-w64-mingw32-windres` (cross-compiling to
// `*-windows-gnu[llvm]`; llvm-mingw ships that wrapper). `RC_<target>` / `RC`
// override it. Best-effort: a missing PNG or a missing resource compiler prints
// a `cargo:warning` and leaves the `.exe` iconless — it never fails the build.
fn embed_windows_resource(out: &Path, workspace_root: &Path) {
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let master = workspace_root.join("assets/icons/AppIcon.png");
    println!("cargo:rerun-if-changed={}", master.display());

    let Ok(bytes) = fs::read(&master) else {
        println!(
            "cargo:warning=dtb-ke-ui: assets/icons/AppIcon.png not found — the Windows .exe will have no icon"
        );
        return;
    };
    let master = match image::load_from_memory(&bytes) {
        Ok(img) => img.to_rgba8(),
        Err(e) => {
            println!("cargo:warning=dtb-ke-ui: AppIcon.png could not be decoded: {e}");
            return;
        }
    };

    // Sizes Explorer / the task bar / Alt-Tab actually ask for.
    const SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];
    let mut images: Vec<(u32, Vec<u8>)> = Vec::new();
    for &size in SIZES {
        let scaled =
            image::imageops::resize(&master, size, size, image::imageops::FilterType::Lanczos3);
        let mut png = Vec::new();
        if image::DynamicImage::ImageRgba8(scaled)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .is_ok()
        {
            images.push((size, png));
        }
    }
    if images.is_empty() {
        println!(
            "cargo:warning=dtb-ke-ui: could not encode any icon size — skipping the .exe icon"
        );
        return;
    }

    // ICONDIR + one directory entry per size + the PNG-compressed bodies
    // (allowed in .ico since Vista; `rc.exe`, `windres` and `llvm-rc` all accept
    // a .ico that already holds them).
    let mut ico = Vec::new();
    ico.extend_from_slice(&0u16.to_le_bytes()); // reserved
    ico.extend_from_slice(&1u16.to_le_bytes()); // type: icon
    ico.extend_from_slice(&(images.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * images.len();
    for (size, png) in &images {
        let dim = if *size >= 256 { 0u8 } else { *size as u8 }; // 0 encodes 256
        ico.push(dim); // width
        ico.push(dim); // height
        ico.push(0); // palette size
        ico.push(0); // reserved
        ico.extend_from_slice(&1u16.to_le_bytes()); // colour planes
        ico.extend_from_slice(&32u16.to_le_bytes()); // bits per pixel
        ico.extend_from_slice(&(png.len() as u32).to_le_bytes());
        ico.extend_from_slice(&(offset as u32).to_le_bytes());
        offset += png.len();
    }
    for (_, png) in &images {
        ico.extend_from_slice(png);
    }

    let ico_path = out.join("app.ico");
    if let Err(e) = fs::write(&ico_path, &ico) {
        println!("cargo:warning=dtb-ke-ui: could not write app.ico: {e}");
        return;
    }

    // `#pragma code_page(65001)` + a UTF-8 file: honoured by both `llvm-rc` and
    // modern GNU `windres`, so the umlaut in the product name survives.
    let version = env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let mut parts = version
        .split(['.', '-', '+'])
        .filter_map(|p| p.parse::<u16>().ok());
    let (v0, v1, v2) = (
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
        parts.next().unwrap_or(0),
    );
    let rc = format!(
        "#pragma code_page(65001)\n\
         1 ICON \"app.ico\"\n\
         \n\
         1 VERSIONINFO\n\
         FILEVERSION {v0},{v1},{v2},0\n\
         PRODUCTVERSION {v0},{v1},{v2},0\n\
         FILEOS 0x40004L\n\
         FILETYPE 0x1L\n\
         {{\n\
         BLOCK \"StringFileInfo\"\n\
         {{\n\
         BLOCK \"040704b0\"\n\
         {{\n\
         VALUE \"CompanyName\", \"Philipp Remy\"\n\
         VALUE \"FileDescription\", \"DTB Kampfrichtereinsatzpläne\"\n\
         VALUE \"FileVersion\", \"{version}\"\n\
         VALUE \"InternalName\", \"DTB-Kampfrichtereinsatzpläne\"\n\
         VALUE \"LegalCopyright\", \"© 2026 Philipp Remy — AGPL-3.0-or-later\"\n\
         VALUE \"OriginalFilename\", \"DTB-Kampfrichtereinsatzpläne.exe\"\n\
         VALUE \"ProductName\", \"DTB Kampfrichtereinsatzpläne\"\n\
         VALUE \"ProductVersion\", \"{version}\"\n\
         }}\n\
         }}\n\
         BLOCK \"VarFileInfo\"\n\
         {{\n\
         VALUE \"Translation\", 0x0407, 0x04B0\n\
         }}\n\
         }}\n"
    );
    let rc_path = out.join("app.rc");
    if let Err(e) = fs::write(&rc_path, rc) {
        println!("cargo:warning=dtb-ke-ui: could not write app.rc: {e}");
        return;
    }

    match embed_resource::compile(&rc_path, embed_resource::NONE).manifest_optional() {
        Ok(()) => {}
        Err(err) => {
            println!("cargo:warning=dtb-ke-ui: embedding the Windows .exe resource failed: {err}")
        }
    }
}

// ── SMTP credentials → an encrypted, obfuscated blob ──────────────────────
//
// The report-transport credentials (`src/mail.rs`) must not appear as clear
// text in the binary or the source tree. `build.rs` reads them from the build
// environment (only ever set on the maintainer's build host / CI secrets),
// encrypts them with a **fresh random ChaCha20 key + nonce** generated here, and
// writes ciphertext + an *obfuscated* key into `$OUT_DIR/smtp_secret.rs`.
//
// This is obfuscation, not protection: a determined reverse-engineer with the
// binary and a debugger can still recover the plaintext (breakpoint where lettre
// receives it, or MITM the SMTP session). It defeats `strings` / grep / casual
// inspection and keeps the credentials out of git and out of this repo's tools.
// The real safety net is the dedicated, send-only mailbox + easy rotation (just
// rebuild with new env values) + server-side rate limiting.
//
// Absent `DTB_KE_SMTP_HOST` / `_USER` / `_PASS` → an empty blob is emitted and
// `mail::config()` returns `None`, so every transmission attempt fails cleanly.
fn emit_smtp_secret(out: &Path) {
    for var in [
        "DTB_KE_SMTP_HOST",
        "DTB_KE_SMTP_PORT",
        "DTB_KE_SMTP_USER",
        "DTB_KE_SMTP_PASS",
        "DTB_KE_SMTP_FROM",
        "DTB_KE_SMTP_TO",
    ] {
        println!("cargo:rerun-if-env-changed={var}");
    }

    let get = |k: &str| env::var(k).ok().filter(|v| !v.trim().is_empty());
    let (host, user, pass) = match (
        get("DTB_KE_SMTP_HOST"),
        get("DTB_KE_SMTP_USER"),
        get("DTB_KE_SMTP_PASS"),
    ) {
        (Some(h), Some(u), Some(p)) => (h, u, p),
        _ => {
            println!(
                "cargo:warning=dtb-ke-ui: DTB_KE_SMTP_{{HOST,USER,PASS}} not set — \
                 crash-report transmission is disabled in this build"
            );
            fs::write(out.join("smtp_secret.rs"), SMTP_SECRET_EMPTY).expect("write smtp_secret.rs");
            return;
        }
    };
    let port = get("DTB_KE_SMTP_PORT").unwrap_or_else(|| "465".into());
    let from = get("DTB_KE_SMTP_FROM").unwrap_or_else(|| user.clone());
    let to = get("DTB_KE_SMTP_TO").unwrap_or_else(|| user.clone());

    // Plaintext: an 8-byte magic (so a bad decrypt is detectable) + the fields,
    // newline-joined (credentials never contain newlines).
    let mut plain = Vec::from(*b"DTBKEM01");
    plain.extend_from_slice([host, port, user, pass, from, to].join("\n").as_bytes());

    // Fresh key + nonce, straight from the OS CSPRNG.
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    getrandom::fill(&mut key).expect("OS randomness for the SMTP key");
    getrandom::fill(&mut nonce).expect("OS randomness for the SMTP nonce");

    let ciphertext = {
        use chacha20::cipher::{KeyIvInit, StreamCipher};
        chacha20::ChaCha20::new((&key).into(), (&nonce).into()).apply_keystream(&mut plain);
        plain
    };

    // The key is split into four `u64` words, each offset by a distinct fixed
    // salt (below, in `mail.rs` — public, just an extra reversal step) so it does
    // not sit in `.rodata` as a recognisable 32-byte high-entropy run. The nonce
    // is XOR-masked the same way.
    const KEY_SALTS: [u64; 4] = [
        0x9E37_79B9_7F4A_7C15,
        0xC2B2_AE3D_27D4_EB4F,
        0x1656_67B1_9E37_79F9,
        0xF58C_4C24_D442_1D1D,
    ];
    const NONCE_MASK: [u8; 12] = [
        0x5A, 0xC3, 0x1F, 0x88, 0x24, 0x9D, 0x71, 0xE6, 0x4B, 0x0A, 0xB2, 0x3C,
    ];

    let word = |i: usize| {
        u64::from_le_bytes(key[i * 8..i * 8 + 8].try_into().unwrap()).wrapping_add(KEY_SALTS[i])
    };
    let masked_nonce: Vec<u8> = nonce.iter().zip(NONCE_MASK).map(|(b, m)| b ^ m).collect();

    let mut s = String::new();
    s.push_str("// @generated by build.rs from DTB_KE_SMTP_* — do not edit.\n");
    s.push_str(&format!(
        "pub const KEY_WORDS: [u64; 4] = [{:#018x}, {:#018x}, {:#018x}, {:#018x}];\n",
        word(0),
        word(1),
        word(2),
        word(3),
    ));
    s.push_str(&format!(
        "pub const NONCE_MASKED: [u8; 12] = {masked_nonce:?};\n"
    ));
    s.push_str(&format!("pub const CIPHERTEXT: &[u8] = &{ciphertext:?};\n"));
    fs::write(out.join("smtp_secret.rs"), s).expect("write smtp_secret.rs");
}

/// The `smtp_secret.rs` emitted when no credentials are configured.
const SMTP_SECRET_EMPTY: &str = "// @generated by build.rs — SMTP not configured.\n\
pub const KEY_WORDS: [u64; 4] = [0, 0, 0, 0];\n\
pub const NONCE_MASKED: [u8; 12] = [0; 12];\n\
pub const CIPHERTEXT: &[u8] = &[];\n";

// ── helpers: generated-code emitters ──────────────────────────────────────

fn put_str(w: &mut String, name: &str, value: &str) {
    w.push_str(&format!("pub const {name}: &str = {value:?};\n"));
}

/// A `&str` const that is `""` when the value is empty/unknown; the module maps
/// `""` to `None` for display.
fn put_opt(w: &mut String, name: &str, value: Option<String>) {
    let v = value.unwrap_or_default();
    w.push_str(&format!("pub const {name}: &str = {v:?};\n"));
}

fn opt_lit(v: Option<&str>) -> String {
    match v {
        Some(s) => format!("Some({s:?})"),
        None => "None".to_string(),
    }
}

fn rerun(path: &str) {
    println!("cargo:rerun-if-changed={path}");
}

// ── toolchain ─────────────────────────────────────────────────────────────

fn rustc_version() -> (Option<String>, Option<String>) {
    let rustc = env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let Ok(out) = Command::new(rustc).arg("-vV").output() else {
        return (None, None);
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let field = |key: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(key))
            .map(|v| v.trim().to_string())
    };
    (field("release:"), field("LLVM version:"))
}

/// A rough identifier for the linker driver (`cc --version` first line, or the
/// `RUSTC_LINKER` override). Best-effort.
fn linker() -> Option<String> {
    if let Ok(l) = env::var("RUSTC_LINKER") {
        if !l.is_empty() {
            return Some(l);
        }
    }
    let cc = env::var("CC").unwrap_or_else(|_| "cc".into());
    let out = Command::new(&cc).arg("--version").output().ok()?;
    let first = String::from_utf8_lossy(&out.stdout);
    first.lines().next().map(|s| s.trim().to_string())
}

// ── profile settings (workspace Cargo.toml `[profile.<name>]`) ─────────────

struct ProfileSettings {
    debug: Option<String>,
    lto: Option<String>,
    codegen_units: Option<String>,
    strip: Option<String>,
    incremental: Option<String>,
}

impl ProfileSettings {
    fn resolve(workspace_root: &Path, profile: &str) -> Self {
        // Cargo's built-in defaults for the two standard profiles.
        let mut s = if profile == "release" {
            Self {
                debug: Some("none".into()),
                lto: Some("false".into()),
                codegen_units: Some("16".into()),
                strip: Some("none".into()),
                incremental: Some("false".into()),
            }
        } else {
            Self {
                debug: Some("full".into()),
                lto: Some("false".into()),
                codegen_units: Some("256".into()),
                strip: Some("none".into()),
                incremental: Some("true".into()),
            }
        };

        let cargo_toml = fs::read_to_string(workspace_root.join("Cargo.toml")).unwrap_or_default();
        let section = if profile == "release" {
            "[profile.release]"
        } else {
            "[profile.dev]"
        };
        if let Some(body) = table_body(&cargo_toml, section) {
            for (k, v) in body {
                match k {
                    "debug" => s.debug = Some(v),
                    "lto" => s.lto = Some(v),
                    "codegen-units" => s.codegen_units = Some(v),
                    "strip" => s.strip = Some(v),
                    "incremental" => s.incremental = Some(v),
                    _ => {}
                }
            }
        }
        // `CARGO_INCREMENTAL` overrides everything.
        if let Ok(i) = env::var("CARGO_INCREMENTAL") {
            s.incremental = Some(if i == "1" { "true" } else { "false" }.into());
        }
        s
    }
}

/// Return the `key = value` pairs of the first `[header]` table, values with
/// quotes and trailing `# comments` stripped. Stops at the next `[`.
fn table_body<'a>(toml: &'a str, header: &str) -> Option<Vec<(&'a str, String)>> {
    let start = toml.find(header)? + header.len();
    let rest = &toml[start..];
    let end = rest.find("\n[").map(|i| i + 1).unwrap_or(rest.len());
    let mut out = Vec::new();
    for line in rest[..end].lines() {
        let line = line.split('#').next().unwrap_or("").trim();
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let v = v.trim().trim_matches('"').trim().to_string();
        if !v.is_empty() {
            out.push((k.trim(), v));
        }
    }
    Some(out)
}

// ── VCS ───────────────────────────────────────────────────────────────────

struct Vcs {
    commit: Option<String>,
    branch: Option<String>,
    commit_date: Option<String>,
    working_tree: Option<String>,
}

impl Vcs {
    fn detect(root: &Path) -> Self {
        let git = |args: &[&str]| -> Option<String> {
            let out = Command::new("git")
                .arg("-C")
                .arg(root)
                .args(args)
                .output()
                .ok()?;
            if !out.status.success() {
                return None;
            }
            let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
            (!s.is_empty()).then_some(s)
        };

        let inside = git(&["rev-parse", "--is-inside-work-tree"]).as_deref() == Some("true");
        if !inside {
            return Self {
                commit: None,
                branch: None,
                commit_date: None,
                working_tree: None,
            };
        }

        Self {
            commit: git(&["rev-parse", "--short=12", "HEAD"]),
            branch: git(&["rev-parse", "--abbrev-ref", "HEAD"]),
            commit_date: git(&["log", "-1", "--format=%cI"]),
            working_tree: git(&["status", "--porcelain"])
                .map(|s| if s.is_empty() { "sauber" } else { "verändert" }.to_string())
                .or(Some("sauber".to_string())),
        }
    }
}

// ── dependency licence collection ─────────────────────────────────────────

struct Package {
    name: String,
    version: String,
    /// `Some(rev)` for a git source, `Some(String::new())` for a registry
    /// source, `None` for a path dependency.
    git_rev: Option<String>,
}

fn parse_lock(lock: &str) -> Vec<Package> {
    let mut out = Vec::new();
    let mut name = None;
    let mut version = None;
    let mut source: Option<String> = None;
    let mut in_pkg = false;

    let flush = |out: &mut Vec<Package>,
                 name: &mut Option<String>,
                 version: &mut Option<String>,
                 source: &mut Option<String>| {
        if let (Some(n), Some(v)) = (name.take(), version.take()) {
            let src = source.take();
            let git_rev = match src.as_deref() {
                None => None,
                Some(s) if s.starts_with("git+") => s.rsplit_once('#').map(|(_, r)| r.to_string()),
                Some(_) => Some(String::new()),
            };
            out.push(Package {
                name: n,
                version: v,
                git_rev,
            });
        }
    };

    for line in lock.lines() {
        if line == "[[package]]" {
            flush(&mut out, &mut name, &mut version, &mut source);
            in_pkg = true;
            continue;
        }
        if !in_pkg {
            continue;
        }
        if let Some(v) = line.strip_prefix("name = ") {
            name = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("version = ") {
            version = Some(v.trim_matches('"').to_string());
        } else if let Some(v) = line.strip_prefix("source = ") {
            source = Some(v.trim_matches('"').to_string());
        }
    }
    flush(&mut out, &mut name, &mut version, &mut source);
    out
}

/// A one-time index of every crate source unpacked under `CARGO_HOME`, so
/// resolving a dependency to its directory is a hashmap lookup instead of a
/// fresh filesystem crawl per dependency. The old per-dependency search was
/// `O(deps × store roots × tree)` — and this project's ~28 git dependencies
/// almost all live in the same very large `zed` checkout, so it re-walked that
/// tree ~28 times (once per dep) plus, for any registry dep whose exact
/// version wasn't unpacked, a full recursive walk of every registry root too.
struct SourceIndex {
    /// `registry/src/<hash>/<name>-<version>/`, keyed by `<name>-<version>`.
    registry_exact: HashMap<String, PathBuf>,
    /// The same, keyed by bare `<name>` — the fallback the old code reached via
    /// a misapplied recursive search when the exact version wasn't unpacked
    /// (common for platform-gated deps like `jni`). Some version of the crate's
    /// licence text is better than none in the About window.
    registry_by_name: HashMap<String, PathBuf>,
    /// `git/checkouts/<repo>/<short-rev>/…/<crate>/`, keyed by the crate's own
    /// `[package] name` (a git checkout can hold a whole workspace). Only the
    /// `<short-rev>` directories the lockfile actually pins are indexed, so a
    /// name resolves to exactly one directory even when several revs of the
    /// same repo are cached side by side.
    git: HashMap<String, PathBuf>,
}

impl SourceIndex {
    fn build(deps: &[&Package]) -> Self {
        let cargo_home = env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .or_else(|| env::var_os("HOME").map(|h| PathBuf::from(h).join(".cargo")))
            .unwrap_or_default();

        // Registry: each entry is already named `<name>-<version>`, so one
        // `read_dir` per index-hash directory is the whole job.
        let mut registry_exact = HashMap::new();
        let mut registry_by_name = HashMap::new();
        if let Ok(hash_dirs) = fs::read_dir(cargo_home.join("registry/src")) {
            for hash_dir in hash_dirs.flatten() {
                if let Ok(entries) = fs::read_dir(hash_dir.path()) {
                    for e in entries.flatten() {
                        let full = e.file_name().to_string_lossy().into_owned();
                        // "<name>-<version>": the version starts at the last
                        // `-` followed by a digit.
                        let name = full
                            .rsplit_once('-')
                            .filter(|(_, v)| v.starts_with(|c: char| c.is_ascii_digit()))
                            .map(|(n, _)| n.to_owned());
                        if let Some(name) = name {
                            registry_by_name.entry(name).or_insert_with(|| e.path());
                        }
                        registry_exact.entry(full).or_insert_with(|| e.path());
                    }
                }
            }
        }

        // Git: the `<repo>/<short-rev>/` directories the lockfile pins. Cargo
        // names `<short-rev>` as a prefix of the full commit hash.
        let repo_dirs: Vec<PathBuf> = fs::read_dir(cargo_home.join("git/checkouts"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect();
        let mut rev_dirs: Vec<PathBuf> = deps
            .iter()
            .filter_map(|p| p.git_rev.as_deref())
            .filter(|r| !r.is_empty())
            .flat_map(|rev| {
                repo_dirs.iter().filter_map(move |repo| {
                    fs::read_dir(repo).ok()?.flatten().find_map(|e| {
                        let name = e.file_name();
                        let name = name.to_str()?;
                        (name.len() >= 7 && rev.starts_with(name)).then(|| e.path())
                    })
                })
            })
            .collect();
        rev_dirs.sort();
        rev_dirs.dedup();

        // Walk those trees once each, in parallel — this is the part that used
        // to dominate the whole script's runtime.
        let git = rev_dirs
            .par_iter()
            .map(|root| {
                let mut local = HashMap::new();
                index_git_dir(root, 3, &mut local);
                local
            })
            .reduce(HashMap::new, |mut acc, m| {
                for (k, v) in m {
                    acc.entry(k).or_insert(v);
                }
                acc
            });

        Self {
            registry_exact,
            registry_by_name,
            git,
        }
    }

    fn locate(&self, p: &Package) -> Option<PathBuf> {
        match p.git_rev.as_deref() {
            Some(rev) if !rev.is_empty() => self.git.get(&p.name).cloned(),
            _ => self
                .registry_exact
                .get(&format!("{}-{}", p.name, p.version))
                .or_else(|| self.registry_by_name.get(&p.name))
                .cloned(),
        }
    }
}

/// Add every `[package]`-bearing directory under `dir` (depth-limited) to
/// `out`, keyed by its declared crate name. `target` subtrees are skipped;
/// the first path seen for a given name wins.
fn index_git_dir(dir: &Path, depth: usize, out: &mut HashMap<String, PathBuf>) {
    let manifest = dir.join("Cargo.toml");
    if manifest.is_file() {
        if let Ok(t) = fs::read_to_string(&manifest) {
            if let Some(name) = manifest_name(&t) {
                out.entry(name).or_insert_with(|| dir.to_path_buf());
            }
        }
    }
    if depth == 0 {
        return;
    }
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() && !p.ends_with("target") {
            index_git_dir(&p, depth - 1, out);
        }
    }
}

fn manifest_name(toml: &str) -> Option<String> {
    // the `name` under the first `[package]` table
    let body = table_body(toml, "[package]")?;
    body.into_iter().find(|(k, _)| *k == "name").map(|(_, v)| v)
}

fn read_spdx(dir: &Path) -> Option<String> {
    let toml = fs::read_to_string(dir.join("Cargo.toml")).ok()?;
    let body = table_body(&toml, "[package]")?;
    body.into_iter()
        .find(|(k, _)| *k == "license")
        .map(|(_, v)| v)
        .filter(|v| !v.is_empty())
}

const LICENSE_FILENAMES: &[&str] = &[
    "LICENSE",
    "LICENSE.md",
    "LICENSE.txt",
    "LICENSE-MIT",
    "LICENSE-MIT.md",
    "LICENSE-MIT.txt",
    "LICENSE-APACHE",
    "LICENSE-APACHE.md",
    "LICENSE-APACHE.txt",
    "LICENSE-BSD",
    "LICENSE-ZLIB",
    "LICENCE",
    "LICENCE.md",
    "LICENCE.txt",
    "COPYING",
    "COPYING.md",
    "COPYING.LESSER",
    "COPYRIGHT",
    "UNLICENSE",
    "UNLICENSE.md",
    "NOTICE",
];

fn read_license_text(dir: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for name in LICENSE_FILENAMES {
        let path = dir.join(name);
        if let Ok(text) = fs::read_to_string(&path) {
            let text = text.trim();
            if !text.is_empty() && !parts.iter().any(|p| p == text) {
                parts.push(format!("── {name} ──\n\n{text}"));
            }
        }
    }
    (!parts.is_empty()).then(|| parts.join("\n\n\n"))
}
