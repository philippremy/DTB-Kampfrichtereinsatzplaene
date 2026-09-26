//! Linux packaging: a relocatable tarball, a `.deb`, a `.rpm` (via `rpmbuild`)
//! and an `.AppImage` (via `appimagetool`).
//!
//! All four share one `/usr`-prefixed payload tree built once by [`stage_prefix`].
//! `.deb` is assembled directly (hand-rolled `ar` + `tar`); `.rpm` and
//! `.AppImage` shell out to their standard tools when present.

use std::path::Path;
use std::process::Command;

use crate::archive;
use crate::bundle::Context;
use crate::util::{copy, fresh_dir, have, report, try_run, workspace_root};
use crate::{icon, meta};

/// Architecture spellings for the target being packaged. Debian and RPM name
/// the same CPU differently (`amd64` vs `x86_64`, `arm64` vs `aarch64`);
/// AppImage's `ARCH` env and the tarball stem follow the RPM/kernel spelling.
struct ArchLabels {
    /// `Architecture:` in the `.deb` control file.
    deb: &'static str,
    /// `BuildArch:` in the `.spec`, the `RPMS/<arch>/` dir, the `.AppImage`
    /// `ARCH` env, and the tarball stem.
    rpm: &'static str,
}

/// Resolve [`ArchLabels`] from `cx.target` (the `--target` triple; `None` = a
/// native build, so the host arch). Errors on an arch we don't have labels for
/// rather than silently mislabelling the package.
fn arch_labels(cx: &Context) -> Result<ArchLabels, String> {
    let arch = match cx.target.as_deref() {
        Some(triple) => triple.split('-').next().unwrap_or(""),
        None => std::env::consts::ARCH,
    };
    match arch {
        "x86_64" => Ok(ArchLabels {
            deb: "amd64",
            rpm: "x86_64",
        }),
        "aarch64" => Ok(ArchLabels {
            deb: "arm64",
            rpm: "aarch64",
        }),
        other => Err(format!(
            "Linux packaging has no arch labels for `{other}` (target {:?}) — \
             add it to linux::arch_labels",
            cx.target
        )),
    }
}

/// Runtime shared libraries gpui needs that aren't part of a base system.
/// Conservative on purpose — too many `Depends` breaks installs on lean
/// systems. Verify with `ldd target/release/<bin>` when in doubt.
const DEB_DEPENDS: &str = "libc6, libgcc-s1, libgl1, libx11-6, libxcb1, \
libxkbcommon0, libwayland-client0, libfontconfig1, libfreetype6";

pub fn bundle(cx: &Context) -> Result<(), String> {
    let prefix = cx.out_dir.join("prefix");
    stage_prefix(cx, &prefix)?;

    let mut made = 0;
    if cx.wants("tar") {
        tarball(cx, &prefix)?;
        made += 1;
    }
    if cx.wants("deb") {
        deb(cx, &prefix)?;
        made += 1;
    }
    if cx.wants("rpm") {
        rpm(cx, &prefix)?;
        made += 1;
    }
    if cx.wants("appimage") {
        appimage(cx, &prefix)?;
        made += 1;
    }
    if made == 0 {
        // Not fatal — `--formats none` is a deliberate sentinel (no real
        // format name matches it) that CI's raw-archive legs pass on purpose
        // to skip packaging entirely and keep just the staged prefix tree
        // for the workflow to tar up itself. Matches how windows.rs skips
        // `.msi` and macos.rs skips `.icns` when they're not wanted.
        eprintln!(
            "dtb-ke-bundle: no Linux formats produced (see --formats) — leaving just the staged prefix"
        );
    }
    Ok(())
}

// ── shared payload ─────────────────────────────────────────────────────────

/// Build the `/usr`-prefixed tree every package installs.
fn stage_prefix(cx: &Context, prefix: &Path) -> Result<(), String> {
    fresh_dir(prefix).map_err(io)?;

    // Executable → /usr/bin/<slug>
    let bin = prefix.join("usr/bin").join(meta::p().slug);
    copy(&cx.binary, &bin).map_err(io)?;
    set_mode(&bin, 0o755)?;

    // .desktop → /usr/share/applications/<id>.desktop
    write(
        &prefix
            .join("usr/share/applications")
            .join(format!("{}.desktop", meta::p().rdns_id)),
        desktop_entry(),
    )?;

    // Icons → /usr/share/icons/hicolor/…
    if cx.have_icon && icon::hicolor_dir().exists() {
        copy_tree(
            &icon::hicolor_dir(),
            &prefix.join("usr/share/icons/hicolor"),
        )?;
    }

    // AppStream metadata → /usr/share/metainfo/<id>.metainfo.xml
    write(
        &prefix
            .join("usr/share/metainfo")
            .join(format!("{}.metainfo.xml", meta::p().rdns_id)),
        metainfo(),
    )?;

    // `.dtbke` MIME type → /usr/share/mime/packages/<id>.xml — shared-mime-info
    // has no notion of a per-app "file association" the way Windows/macOS do;
    // this declares the type itself (name, glob, icon lookup name), and the
    // `.desktop` file's `MimeType=` (above) is what actually associates this
    // app with it. `update-mime-database` (install scripts below) must run
    // afterwards for the XDG shared-mime cache to pick it up.
    write(
        &prefix
            .join("usr/share/mime/packages")
            .join(format!("{}.xml", meta::p().rdns_id)),
        mime_package(),
    )?;

    // Licence → /usr/share/doc/<slug>/copyright
    copy(
        &workspace_root().join(meta::p().license_file),
        &prefix
            .join("usr/share/doc")
            .join(meta::p().slug)
            .join("copyright"),
    )
    .ok();

    Ok(())
}

fn desktop_entry() -> String {
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Version=1.0\n\
         Name={name}\n\
         Comment={summary}\n\
         Exec={slug} %F\n\
         Icon={id}\n\
         Terminal=false\n\
         Categories={categories}\n\
         MimeType={mime};\n\
         StartupWMClass={wmclass}\n\
         StartupNotify=true\n",
        name = meta::p().display_name,
        summary = meta::p().summary,
        slug = meta::p().slug,
        id = meta::p().rdns_id,
        categories = meta::p().freedesktop_categories,
        mime = meta::p().doc_mime_type,
        // Kept as the raw kebab-case name, not meta::p().display_name — a WM_CLASS
        // with a space in it is unconventional and this preserves the exact
        // prior behavior (this field's *correct* value — whatever gpui
        // actually sets as the X11 WM_CLASS hint at runtime — hasn't been
        // independently verified; flagging rather than guessing further).
        wmclass = meta::p().raw_bin_name,
    )
}

/// The shared-mime-info package declaring `application/x-dtbke` — name,
/// `*.dtbke` glob, and (implicitly) its icon: with no `<icon>` override, XDG
/// icon-theme lookup for this type tries `application-x-dtbke` (the type
/// name with `/` → `-`) automatically, which is exactly the name
/// `icon::DOC_MIME_ICON_NAME` writes into `hicolor/<size>/mimetypes/` under.
fn mime_package() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<mime-info xmlns="http://www.freedesktop.org/standards/shared-mime-info">
  <mime-type type="{mime}">
    <comment>{name}</comment>
    <glob pattern="*.{ext}"/>
  </mime-type>
</mime-info>
"#,
        mime = meta::p().doc_mime_type,
        name = xml(meta::p().doc_type_name),
        ext = meta::p().doc_extension,
    )
}

fn metainfo() -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<component type="desktop-application">
  <id>{id}</id>
  <metadata_license>CC0-1.0</metadata_license>
  <project_license>{license}</project_license>
  <name>{name}</name>
  <summary>{summary}</summary>
  <description>
    <p>{description}</p>
  </description>
  <launchable type="desktop-id">{id}.desktop</launchable>
  <url type="homepage">{homepage}</url>
  <content_rating type="oars-1.1" />
  <developer id="de.philippremy">
    <name>{publisher}</name>
  </developer>
  <provides>
    <mediatype>{mime}</mediatype>
  </provides>
  <releases>
    <release version="{version}" date="{date}" />
  </releases>
</component>
"#,
        id = meta::p().rdns_id,
        license = meta::LICENSE,
        name = xml(meta::p().display_name),
        summary = xml(meta::p().summary),
        description = xml(meta::p().description),
        homepage = xml(meta::HOMEPAGE),
        publisher = xml(meta::PUBLISHER),
        mime = meta::p().doc_mime_type,
        version = meta::numeric_version(),
        // `appstreamcli validate` (run by appimagetool, fatal on error)
        // rejects a `<release>` with no `date`/`timestamp`.
        date = chrono::Utc::now().format("%Y-%m-%d"),
    )
}

// ── tarball ────────────────────────────────────────────────────────────────

fn tarball(cx: &Context, prefix: &Path) -> Result<(), String> {
    let arch = arch_labels(cx)?;
    let stem = format!(
        "{}-{}-{}",
        meta::p().slug,
        meta::numeric_version(),
        arch.rpm
    );
    let root = cx.out_dir.join(&stem);
    fresh_dir(&root).map_err(io)?;
    copy_tree(&prefix.join("usr"), &root.join("usr"))?;
    write(&root.join("install.sh"), install_script(true))?;
    write(&root.join("uninstall.sh"), install_script(false))?;
    set_mode(&root.join("install.sh"), 0o755)?;
    set_mode(&root.join("uninstall.sh"), 0o755)?;

    let out = cx.out_dir.join(format!("{stem}.tar.gz"));
    archive::targz(&cx.out_dir, &[stem.as_str()], &out)?;
    crate::util::remove(&root).ok();
    report(&out);
    Ok(())
}

fn install_script(install: bool) -> String {
    if install {
        "#!/bin/sh\n\
         # Copy the tree into a prefix (default /usr/local) and refresh caches.\n\
         set -e\n\
         PREFIX=\"${1:-/usr/local}\"\n\
         echo \"installing to $PREFIX (needs write permission)\"\n\
         cp -a usr/. \"$PREFIX/\"\n\
         # /usr/bin/<slug> was built for /usr; fix the Exec if a custom prefix.\n\
         update-desktop-database \"$PREFIX/share/applications\" 2>/dev/null || true\n\
         update-mime-database \"$PREFIX/share/mime\" 2>/dev/null || true\n\
         gtk-update-icon-cache \"$PREFIX/share/icons/hicolor\" 2>/dev/null || true\n\
         echo done\n"
            .to_string()
    } else {
        format!(
            "#!/bin/sh\n\
             set -e\n\
             PREFIX=\"${{1:-/usr/local}}\"\n\
             rm -f \"$PREFIX/bin/{slug}\"\n\
             rm -f \"$PREFIX/share/applications/{id}.desktop\"\n\
             rm -f \"$PREFIX/share/metainfo/{id}.metainfo.xml\"\n\
             rm -f \"$PREFIX/share/mime/packages/{id}.xml\"\n\
             find \"$PREFIX/share/icons/hicolor\" \\( -name '{id}.*' -o -name '{doc_icon}.*' \\) \
-delete 2>/dev/null || true\n\
             rm -rf \"$PREFIX/share/doc/{slug}\"\n\
             update-mime-database \"$PREFIX/share/mime\" 2>/dev/null || true\n\
             echo done\n",
            slug = meta::p().slug,
            id = meta::p().rdns_id,
            doc_icon = icon::doc_mime_icon_name(),
        )
    }
}

// ── .deb ───────────────────────────────────────────────────────────────────

fn deb(cx: &Context, prefix: &Path) -> Result<(), String> {
    let arch = arch_labels(cx)?;
    let work = cx.out_dir.join("deb");
    fresh_dir(&work).map_err(io)?;

    // data.tar.gz — the payload; `.` so paths come out as `./usr/…` (dpkg's
    // canonical form).
    let data_tgz = work.join("data.tar.gz");
    archive::targz(prefix, &[], &data_tgz)?;

    // control.tar.gz — control + md5sums + maintainer scripts
    let ctl_dir = work.join("control");
    std::fs::create_dir_all(&ctl_dir).map_err(io)?;
    let installed_kb = dir_size_kb(&prefix.join("usr"));
    write(
        &ctl_dir.join("control"),
        deb_control(installed_kb, arch.deb),
    )?;
    write(&ctl_dir.join("md5sums"), deb_md5sums(prefix)?)?;
    write(&ctl_dir.join("postinst"), maintainer_script())?;
    write(&ctl_dir.join("postrm"), maintainer_script())?;
    set_mode(&ctl_dir.join("postinst"), 0o755)?;
    set_mode(&ctl_dir.join("postrm"), 0o755)?;
    let ctl_tgz = work.join("control.tar.gz");
    archive::targz(&ctl_dir, &[], &ctl_tgz)?;

    let out = cx.out_dir.join(format!(
        "{}_{}_{}.deb",
        meta::p().slug,
        meta::numeric_version(),
        arch.deb
    ));
    archive::ar_deb(&out, &ctl_tgz, &data_tgz)?;
    crate::util::remove(&work).ok();
    report(&out);
    Ok(())
}

fn deb_control(installed_kb: u64, arch: &str) -> String {
    format!(
        "Package: {pkg}\n\
         Version: {version}\n\
         Architecture: {arch}\n\
         Maintainer: {publisher}\n\
         Installed-Size: {size}\n\
         Depends: {depends}\n\
         Section: x11\n\
         Priority: optional\n\
         Homepage: {homepage}\n\
         Description: {summary}\n\
         {desc_body}\n",
        pkg = meta::p().slug,
        version = meta::numeric_version(),
        arch = arch,
        publisher = meta::PUBLISHER,
        size = installed_kb,
        depends = DEB_DEPENDS,
        homepage = meta::HOMEPAGE,
        summary = meta::p().summary,
        desc_body = fold_description(meta::p().description),
    )
}

/// Debian long-description: each line prefixed with a single space, blank lines
/// become " .".
fn fold_description(text: &str) -> String {
    text.split('\n')
        .map(|l| {
            let l = l.trim();
            if l.is_empty() {
                " .".to_string()
            } else {
                format!(" {l}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn deb_md5sums(prefix: &Path) -> Result<String, String> {
    // `md5sum` (coreutils) is on every Debian build host.
    let out = Command::new("sh")
        .arg("-c")
        .arg("find usr -type f -print0 | xargs -0 md5sum")
        .current_dir(prefix)
        .output()
        .map_err(|e| format!("md5sum: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "md5sum failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

fn maintainer_script() -> String {
    "#!/bin/sh\n\
     set -e\n\
     if [ -x \"$(command -v update-desktop-database)\" ]; then\n\
       update-desktop-database -q /usr/share/applications || true\n\
     fi\n\
     if [ -x \"$(command -v update-mime-database)\" ]; then\n\
       update-mime-database /usr/share/mime || true\n\
     fi\n\
     if [ -x \"$(command -v gtk-update-icon-cache)\" ]; then\n\
       gtk-update-icon-cache -q -t -f /usr/share/icons/hicolor || true\n\
     fi\n"
        .to_string()
}

// ── .rpm ───────────────────────────────────────────────────────────────────

fn rpm(cx: &Context, prefix: &Path) -> Result<(), String> {
    if !have("rpmbuild") {
        eprintln!(
            "dtb-ke-bundle: `rpmbuild` not on PATH — skipping .rpm \
             (install `rpm-build` / `rpm` and re-run)"
        );
        return Ok(());
    }

    let arch = arch_labels(cx)?;
    let top = cx.out_dir.join("rpmbuild");
    for sub in ["SPECS", "SOURCES", "BUILD", "BUILDROOT", "RPMS", "SRPMS"] {
        std::fs::create_dir_all(top.join(sub)).map_err(io)?;
    }

    // Sources: a tarball of the usr tree that %install unpacks into the buildroot.
    let src_stem = format!("{}-{}", meta::p().slug, meta::numeric_version());
    let src_tgz = top.join("SOURCES").join(format!("{src_stem}.tar.gz"));
    // Repack `usr` under a versioned top dir so %setup is happy.
    let repack = cx.out_dir.join("rpm-src").join(&src_stem);
    fresh_dir(&repack).map_err(io)?;
    copy_tree(&prefix.join("usr"), &repack.join("usr"))?;
    archive::targz(&cx.out_dir.join("rpm-src"), &[src_stem.as_str()], &src_tgz)?;
    crate::util::remove(&cx.out_dir.join("rpm-src")).ok();

    let spec = top.join("SPECS").join(format!("{}.spec", meta::p().slug));
    write(&spec, rpm_spec(&src_stem))?;

    // Arch/CachyOS (the CI host) has no populated system rpm database, so
    // `rpmbuild` prints "cannot open Packages database in /var/lib/rpm".
    // Point it at a throwaway db inside the build tree instead — `rpm
    // --initdb` makes it, then `_dbpath` tells rpmbuild to use it. If `rpm`
    // isn't on PATH the warning just stays (it's non-fatal on its own).
    let topdir = format!("_topdir {}", top.display());
    let mut rpm_args: Vec<String> = vec!["--define".into(), topdir];
    let rpmdb = top.join("rpmdb");
    if have("rpm") && std::fs::create_dir_all(&rpmdb).is_ok() {
        let dbpath = rpmdb.to_string_lossy().into_owned();
        try_run("rpm", &["--initdb", "--dbpath", &dbpath], &workspace_root()).ok();
        rpm_args.push("--define".into());
        rpm_args.push(format!("_dbpath {dbpath}"));
    }
    // `--target <arch>` (rpm-common(8): PLATFORM is `arch[-os]`, os optional)
    // makes rpm use that arch's config instead of autodetecting the host, so
    // the aarch64 leg cross-builds on the x86_64 CI host without the "No
    // compatible architectures found for build" refusal. It's also the *only*
    // thing setting the package arch — the spec has no `BuildArch` (mixing the
    // two is documented undefined behavior). No-op when arch == host.
    rpm_args.push("--target".into());
    rpm_args.push(arch.rpm.to_string());
    rpm_args.push("-bb".into());
    rpm_args.push(spec.to_string_lossy().into_owned());

    try_run(
        "rpmbuild",
        &rpm_args.iter().map(String::as_str).collect::<Vec<_>>(),
        &workspace_root(),
    )?;

    // Move the built rpm out of RPMS/<arch>/.
    let built = top.join("RPMS").join(arch.rpm);
    if let Ok(entries) = std::fs::read_dir(&built) {
        for e in entries.flatten() {
            if e.path().extension().is_some_and(|x| x == "rpm") {
                let dest = cx.out_dir.join(e.file_name());
                std::fs::rename(e.path(), &dest).map_err(io)?;
                report(&dest);
            }
        }
    }
    crate::util::remove(&top).ok();
    Ok(())
}

fn rpm_spec(src_stem: &str) -> String {
    format!(
        // The binary ships pre-stripped ([profile.release] strip = "symbols";
        // debug info goes to a separate .dwp, never into the payload), so
        // rpm's automatic `-debuginfo` subpackage has nothing to collect and
        // `rpmbuild` dies with "Empty %files file … debugfiles.list". Turn it
        // off.
        //
        // No `BuildArch:` — the package arch comes solely from `rpmbuild
        // --target <cpu>` (see `rpm()`). rpmbuild(1) BUGS: a `--target` that
        // disagrees with a spec `BuildArch` is *undefined behavior*, so we
        // use exactly one mechanism.
        "%global debug_package %{{nil}}\n\
         \n\
         Name:           {pkg}\n\
         Version:        {version}\n\
         Release:        1%{{?dist}}\n\
         Summary:        {summary}\n\
         License:        {license}\n\
         URL:            {homepage}\n\
         Source0:        {src_stem}.tar.gz\n\
         Requires:       hicolor-icon-theme\n\
         Requires:       shared-mime-info\n\
         \n\
         %description\n\
         {description}\n\
         \n\
         %prep\n\
         %setup -q -n {src_stem}\n\
         \n\
         %build\n\
         \n\
         %install\n\
         mkdir -p %{{buildroot}}/usr\n\
         cp -a usr/. %{{buildroot}}/usr/\n\
         \n\
         %files\n\
         /usr/bin/{pkg}\n\
         /usr/share/applications/{id}.desktop\n\
         /usr/share/icons/hicolor/*\n\
         /usr/share/metainfo/{id}.metainfo.xml\n\
         /usr/share/mime/packages/{id}.xml\n\
         /usr/share/doc/{pkg}/copyright\n\
         \n\
         %post\n\
         touch --no-create /usr/share/icons/hicolor &>/dev/null || :\n\
         \n\
         %postun\n\
         if [ $1 -eq 0 ] ; then\n\
             touch --no-create /usr/share/icons/hicolor &>/dev/null || :\n\
             gtk-update-icon-cache /usr/share/icons/hicolor &>/dev/null || :\n\
             update-mime-database /usr/share/mime &>/dev/null || :\n\
         fi\n\
         update-desktop-database &>/dev/null || :\n\
         \n\
         %posttrans\n\
         gtk-update-icon-cache /usr/share/icons/hicolor &>/dev/null || :\n\
         update-desktop-database &>/dev/null || :\n\
         update-mime-database /usr/share/mime &>/dev/null || :\n\
         \n\
         %changelog\n",
        pkg = meta::p().slug,
        version = meta::numeric_version(),
        summary = meta::p().summary,
        license = meta::LICENSE,
        homepage = meta::HOMEPAGE,
        src_stem = src_stem,
        description = meta::p().description,
        id = meta::p().rdns_id,
    )
}

// ── .AppImage ──────────────────────────────────────────────────────────────

fn appimage(cx: &Context, prefix: &Path) -> Result<(), String> {
    let arch = arch_labels(cx)?;
    let appdir = cx.out_dir.join(format!("{}.AppDir", meta::p().slug));
    fresh_dir(&appdir).map_err(io)?;
    copy_tree(&prefix.join("usr"), &appdir.join("usr"))?;

    // appimagetool only looks for the *legacy* AppStream name
    // `<id>.appdata.xml` (still the one the AppImage docs prescribe), not the
    // modern `<id>.metainfo.xml` we ship everywhere else — AppImage/
    // appimagetool#77. Rename it inside the AppDir so the check passes; the
    // `.deb`/`.rpm`/tarball keep `.metainfo.xml` (what distro tooling wants).
    let meta_dir = appdir.join("usr/share/metainfo");
    let modern = meta_dir.join(format!("{}.metainfo.xml", meta::p().rdns_id));
    if modern.exists() {
        let legacy = meta_dir.join(format!("{}.appdata.xml", meta::p().rdns_id));
        std::fs::rename(&modern, &legacy).map_err(io)?;
    }

    // AppRun launcher.
    let apprun = appdir.join("AppRun");
    write(
        &apprun,
        format!(
            "#!/bin/sh\n\
             HERE=$(dirname \"$(readlink -f \"$0\")\")\n\
             exec \"$HERE/usr/bin/{slug}\" \"$@\"\n",
            slug = meta::p().slug
        ),
    )?;
    set_mode(&apprun, 0o755)?;

    // Top-level .desktop + icon (+ .DirIcon) — appimagetool expects them here.
    copy(
        &appdir
            .join("usr/share/applications")
            .join(format!("{}.desktop", meta::p().rdns_id)),
        &appdir.join(format!("{}.desktop", meta::p().rdns_id)),
    )
    .map_err(io)?;

    if cx.have_icon {
        let png = icon::png_512_path();
        if png.exists() {
            copy(&png, &appdir.join(format!("{}.png", meta::p().rdns_id))).map_err(io)?;
            copy(&png, &appdir.join(".DirIcon")).map_err(io)?;
        }
    }

    if !have("appimagetool") {
        eprintln!(
            "dtb-ke-bundle: `appimagetool` not on PATH — wrote {} but did not build the .AppImage.\n\
             Get it from https://github.com/AppImage/appimagetool/releases and re-run.",
            appdir.display()
        );
        return Ok(());
    }

    let out = cx.out_dir.join(format!(
        "{}-{}-{}.AppImage",
        meta::p().display_name.replace(' ', "_"),
        meta::numeric_version(),
        arch.rpm
    ));
    // ARCH env is required by appimagetool for the runtime it embeds.
    let status = Command::new("appimagetool")
        .env("ARCH", arch.rpm)
        .arg(&appdir)
        .arg(&out)
        .current_dir(&cx.out_dir)
        .status()
        .map_err(|e| format!("appimagetool: {e}"))?;
    if !status.success() {
        return Err(format!("appimagetool exited with {status}"));
    }
    crate::util::remove(&appdir).ok();
    report(&out);
    Ok(())
}

// ── helpers ────────────────────────────────────────────────────────────────

fn dir_size_kb(dir: &Path) -> u64 {
    fn walk(dir: &Path, total: &mut u64) {
        if let Ok(entries) = std::fs::read_dir(dir) {
            for e in entries.flatten() {
                let p = e.path();
                if p.is_dir() {
                    walk(&p, total);
                } else if let Ok(m) = e.metadata() {
                    *total += m.len();
                }
            }
        }
    }
    let mut total = 0;
    walk(dir, &mut total);
    total.div_ceil(1024)
}

/// Recursively copy a directory tree, preserving executable bits.
fn copy_tree(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::create_dir_all(to).map_err(io)?;
    for entry in std::fs::read_dir(from).map_err(io)? {
        let entry = entry.map_err(io)?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_tree(&src, &dst)?;
        } else {
            copy(&src, &dst).map_err(io)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if let Ok(m) = entry.metadata() {
                    let mode = m.permissions().mode();
                    std::fs::set_permissions(&dst, std::fs::Permissions::from_mode(mode)).ok();
                }
            }
        }
    }
    Ok(())
}

fn write(path: &Path, contents: impl AsRef<[u8]>) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(io)?;
    }
    std::fs::write(path, contents).map_err(io)
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)).map_err(io)
}
#[cfg(not(unix))]
fn set_mode(_: &Path, _: u32) -> Result<(), String> {
    Ok(())
}

fn xml(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn io<E: std::fmt::Display>(e: E) -> String {
    e.to_string()
}

#[cfg(test)]
mod tests {
    #[test]
    fn metainfo_names_the_ascii_component_id() {
        let xml = super::metainfo();
        let id_line = xml.lines().find(|l| l.contains("<id>")).unwrap();
        assert_eq!(
            id_line.trim(),
            "<id>de.philippremy.DTB-Kampfrichtereinsatzplaene</id>"
        );
        assert!(
            id_line.is_ascii(),
            "component id must be ASCII for appstreamcli"
        );
        // appstreamcli fails a <release> with no date; it must be ISO YYYY-MM-DD.
        let date = xml
            .split_once("date=\"")
            .and_then(|(_, rest)| rest.split_once('"'))
            .map(|(d, _)| d)
            .expect("release has a date attribute");
        assert!(
            date.len() == 10 && date.split('-').count() == 3,
            "date {date:?} is not YYYY-MM-DD"
        );
    }

    #[test]
    fn metainfo_declares_the_dtbke_mediatype() {
        let xml = super::metainfo();
        assert!(xml.contains("<mediatype>application/x-dtbke</mediatype>"));
    }

    #[test]
    fn mime_package_declares_the_glob() {
        let xml = super::mime_package();
        assert!(xml.contains(r#"type="application/x-dtbke""#));
        assert!(xml.contains(r#"<glob pattern="*.dtbke"/>"#));
        assert_eq!(
            xml.matches('<').count(),
            xml.matches('>').count(),
            "unbalanced tags"
        );
    }

    #[test]
    fn desktop_entry_registers_the_mime_type() {
        let entry = super::desktop_entry();
        assert!(entry.lines().any(|l| l == "MimeType=application/x-dtbke;"));
        assert!(entry.contains("Exec=dtb-ke-kampfrichtereinsatzplaene %F"));
    }
}
