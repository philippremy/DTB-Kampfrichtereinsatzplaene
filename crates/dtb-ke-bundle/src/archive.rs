//! Archive helpers — gzip'd tarballs (via the system `tar`) and a hand-rolled
//! `ar` container for `.deb`.
//!
//! The Linux packagers that use these only ever run on a Linux host, so GNU
//! `tar` conventions are assumed.

use std::path::Path;
use std::process::Command;

/// `tar -czf <out> -C <dir> <entries…>` with root:root ownership and a fixed
/// mtime, so the archive is stable between runs.
pub fn targz(dir: &Path, entries: &[&str], out: &Path) -> Result<(), String> {
    let mut cmd = Command::new("tar");
    cmd.arg("--format=gnu")
        .arg("--owner=root:0")
        .arg("--group=root:0")
        .arg("--mtime=@1704067200") // 2024-01-01, deterministic
        .arg("--sort=name")
        .arg("-czf")
        .arg(out)
        .arg("-C")
        .arg(dir);
    if entries.is_empty() {
        cmd.arg(".");
    } else {
        cmd.args(entries);
    }
    let status = cmd
        .status()
        .map_err(|e| format!("failed to spawn tar: {e}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("tar exited with {status}"))
    }
}

/// Write a Debian `ar` archive: `debian-binary`, then `control.tar.gz`, then
/// `data.tar.gz`, in that order (dpkg requires it).
pub fn ar_deb(out: &Path, control_targz: &Path, data_targz: &Path) -> Result<(), String> {
    let mut buf = Vec::new();
    buf.extend_from_slice(b"!<arch>\n");

    ar_member(&mut buf, "debian-binary", b"2.0\n");
    ar_member(
        &mut buf,
        "control.tar.gz",
        &std::fs::read(control_targz).map_err(|e| format!("read control.tar.gz: {e}"))?,
    );
    ar_member(
        &mut buf,
        "data.tar.gz",
        &std::fs::read(data_targz).map_err(|e| format!("read data.tar.gz: {e}"))?,
    );

    std::fs::write(out, buf).map_err(|e| format!("write {}: {e}", out.display()))
}

/// One `ar` member: a 60-byte ASCII header then the payload, padded to an even
/// length with a newline.
fn ar_member(buf: &mut Vec<u8>, name: &str, data: &[u8]) {
    // name(16) mtime(12) uid(6) gid(6) mode(8) size(10) magic(2)
    let header = format!(
        "{name:<16}{mtime:<12}{uid:<6}{gid:<6}{mode:<8}{size:<10}`\n",
        name = name,
        mtime = 1_704_067_200u64,
        uid = 0,
        gid = 0,
        mode = "100644",
        size = data.len(),
    );
    debug_assert_eq!(header.len(), 60, "ar header must be 60 bytes");
    buf.extend_from_slice(header.as_bytes());
    buf.extend_from_slice(data);
    if data.len() % 2 == 1 {
        buf.push(b'\n');
    }
}
