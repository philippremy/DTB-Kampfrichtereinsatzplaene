//! The `.session` sidecar writer (normal, allocating code — runs at launch and when images load).

use super::wire::*;
use super::*;

fn record(out: &mut Vec<u8>, tag: u16, body: &[u8]) {
    out.extend_from_slice(&tag.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(body);
}

fn kv(out: &mut Vec<u8>, key: &str, value: &str) {
    let mut body = Vec::with_capacity(key.len() + value.len() + 2);
    body.extend_from_slice(key.as_bytes());
    body.push(0);
    body.extend_from_slice(value.as_bytes());
    body.push(0);
    record(out, TAG_KV, &body);
}

/// Serialise a session. `session.modules` should list the main executable first.
pub fn encode(session: &SessionDTO) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(SESSION_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let mut header = Vec::with_capacity(24);
    header.extend_from_slice(&session.launch_id);
    header.extend_from_slice(&session.pid.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    record(&mut out, TAG_SESSION_HEADER, &header);

    kv(&mut out, "app_version", &session.app_version);
    kv(&mut out, "os_version", &session.os_version);
    kv(&mut out, "os_build", &session.os_build);
    kv(&mut out, "machine", &session.machine);
    kv(&mut out, "ncpu", &session.ncpu.to_string());
    kv(&mut out, "exe_path", &session.exe_path);
    kv(&mut out, "build_info", &session.build_info);

    for m in &session.modules {
        let mut body = Vec::with_capacity(MODULE_FIXED + m.path.len());
        body.extend_from_slice(&m.base.to_le_bytes());
        body.extend_from_slice(&m.size.to_le_bytes());
        body.extend_from_slice(&m.uuid);
        body.extend_from_slice(&(if m.is_main { MODULE_FLAG_MAIN } else { 0 }).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(m.path.as_bytes());
        record(&mut out, TAG_MODULE, &body);
    }
    out
}

/// Write atomically (temp file + rename) so a crash mid-write never leaves half a sidecar.
pub fn write(path: &std::path::Path, session: &SessionDTO) -> std::io::Result<()> {
    let tmp = path.with_extension("session.tmp");
    std::fs::write(&tmp, encode(session))?;
    std::fs::rename(&tmp, path)
}

/// Serialise a crash file. The real writer is the signal-safe one in `apple/`; this allocating twin
/// produces the identical bytes and exists for tests and tooling.
pub fn encode_crash(snap: &CrashSnapshotDTO) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(CRASH_MAGIC);
    out.extend_from_slice(&FORMAT_VERSION.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());

    let mut header = Vec::with_capacity(HEADER_PAYLOAD);
    header.extend_from_slice(&snap.launch_id);
    header.extend_from_slice(&snap.pid.to_le_bytes());
    header.extend_from_slice(&0u32.to_le_bytes());
    header.extend_from_slice(&snap.crash_time_unix.to_le_bytes());
    record(&mut out, TAG_HEADER, &header);

    if let Some(e) = &snap.exception {
        let mut body = Vec::with_capacity(EXCEPTION_PAYLOAD);
        body.extend_from_slice(&e.kind.to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        body.extend_from_slice(&e.code.to_le_bytes());
        body.extend_from_slice(&e.subcode.to_le_bytes());
        body.extend_from_slice(&e.thread_id.to_le_bytes());
        record(&mut out, TAG_EXCEPTION, &body);
    }
    if let Some(m) = &snap.panic_message {
        record(&mut out, TAG_PANIC, m.as_bytes());
    }
    for t in &snap.threads {
        let mut body = Vec::with_capacity(THREAD_PAYLOAD);
        body.extend_from_slice(&t.id.to_le_bytes());
        body.extend_from_slice(&(if t.crashed { THREAD_FLAG_CRASHED } else { 0 }).to_le_bytes());
        body.extend_from_slice(&0u32.to_le_bytes());
        for x in t.regs.x {
            body.extend_from_slice(&x.to_le_bytes());
        }
        for v in [t.regs.fp, t.regs.lr, t.regs.sp, t.regs.pc, t.regs.cpsr as u64] {
            body.extend_from_slice(&v.to_le_bytes());
        }
        let mut name = [0u8; THREAD_NAME_LEN];
        let n = t.name.len().min(THREAD_NAME_LEN - 1);
        name[..n].copy_from_slice(&t.name.as_bytes()[..n]);
        body.extend_from_slice(&name);
        record(&mut out, TAG_THREAD, &body);
        if let Some(s) = &t.stack {
            let mut body = Vec::with_capacity(16 + s.data.len());
            body.extend_from_slice(&t.id.to_le_bytes());
            body.extend_from_slice(&s.address.to_le_bytes());
            body.extend_from_slice(&s.data);
            record(&mut out, TAG_STACK, &body);
        }
    }
    if snap.complete {
        record(&mut out, TAG_END, &[]);
    }
    out
}
