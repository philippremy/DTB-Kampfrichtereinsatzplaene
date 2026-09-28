//! Digest a captured dump into everything the crash reporter needs, and persist it.
//!
//! [`build_report`] is pure — no spawn/exec at all — since every platform now self-relaunches and
//! shows the reporter dialog itself, in the same process (see `dtb-ke-crash`'s module doc comment).

use std::time::{SystemTime, UNIX_EPOCH};

/// Everything the crash reporter dialog needs about one captured dump, short of the panic message
/// (the caller already has that from wherever it read the crash parameters) — pure data, built by
/// [`build_report`], with no process spawned and nothing exec'd.
pub struct Report {
    /// Where the dump was written (`<dump_dir>/<slug>-<secs>-<pid>.dtbkedmp`) — computed even if the
    /// write itself failed (see `persisted`), since callers use it as an identifier either way.
    pub dump_path: String,
    /// Whether the write actually succeeded. Only a persisted dump should later be deleted.
    pub persisted: bool,
    /// The dump's bytes, with the `buildinfo`/`nsexception` streams already appended.
    pub minidump: Vec<u8>,
    /// Human-readable crash reason (`minidump::CrashReason` `Display`, e.g. `EXC_BAD_ACCESS /
    /// KERN_INVALID_ADDRESS`), or empty if unknown.
    pub reason: String,
    /// Faulting address, `0x…`, or empty.
    pub address: String,
    /// Crashing thread — `«name» (0x…)` or `0x…`, or empty.
    pub thread: String,
    /// Symbol-free walk of the crashing thread + the module table — no process memory, safe to
    /// e-mail.
    pub stack: String,
    /// Every thread's frames as (image UUID, offset) — the input to the system-symbol hints.
    pub frames: Vec<crate::syshints::FrameRef>,
    /// The OS build the dump ran on (`26A428`), or empty.
    pub os_build: String,
}

/// Appends the `buildinfo`/`nsexception` streams, persists the dump under `dump_dir`, and digests it.
/// Pure — never spawns or execs anything, so every self-relaunched platform can call this directly and
/// go straight into showing the reporter dialog in the same process.
pub fn build_report(
    dump_dir: &str,
    slug: &str,
    pid: i64,
    mut dmp: Vec<u8>,
    build_info: &str,
    nsexception: &str,
) -> Report {
    // The build-info user stream, so a debugger can tell exactly which build crashed. A failure
    // here must never cost us the dump itself.
    if !build_info.is_empty() {
        let _ = crate::patch::append_stream(&mut dmp, crate::buildinfo::STREAM_TYPE, build_info.as_bytes());
    }
    // The uncaught-`NSException` user stream (macOS only, empty for every other fault kind —
    // see `crate::nsexception`).
    if !nsexception.is_empty() {
        let _ = crate::patch::append_stream(&mut dmp, crate::nsexception::STREAM_TYPE, nsexception.as_bytes());
    }

    let secs = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    let dump_path = format!("{dump_dir}/{slug}-{secs}-{pid}.{}", crate::DUMP_EXTENSION);
    // Persist first — we are the reliable actor; a reporter (in-process or a relaunch) may never
    // manage to show anything (headless, GUI init failure).
    let persisted = write_file(&dump_path, &dmp);

    let parsed = minidump::Minidump::read(&dmp[..]).ok();
    let (reason, address, thread) = parsed
        .as_ref()
        .and_then(|dump| {
            let sys = dump.get_stream::<minidump::MinidumpSystemInfo>().ok()?;
            let exc = dump.get_stream::<minidump::MinidumpException>().ok()?;
            let tid = exc.get_crashing_thread_id();
            let name = dump
                .get_stream::<minidump::MinidumpThreadNames>()
                .ok()
                .and_then(|n| n.get_name(tid).map(|c| c.into_owned()))
                .filter(|s| !s.is_empty());
            let thread = match name {
                Some(n) => format!("«{n}» ({tid:#x})"),
                None => format!("{tid:#x}"),
            };
            Some((
                with_signal_name(exc.get_crash_reason(sys.os, sys.cpu).to_string(), &exc.raw.exception_record),
                format!("{:#018x}", exc.get_crash_address(sys.os, sys.cpu)),
                thread,
            ))
        })
        .unwrap_or_default();
    let stack = parsed.as_ref().map(|dump| stack_summary(dump)).unwrap_or_default();
    // Every thread's frames as (image UUID, offset) for the reporter to name system-library frames with.
    let frames = parsed.as_ref().map(all_frames).unwrap_or_default();
    let os_build = parsed
        .as_ref()
        .and_then(|dump| dump.get_stream::<minidump::MinidumpSystemInfo>().ok())
        .and_then(|sys| sys.csd_version().map(|b| b.trim().to_owned()))
        .filter(|b| b.len() <= 12 && !b.contains(' '))
        .unwrap_or_default();

    Report { dump_path, persisted, minidump: dmp, reason, address, thread, stack, frames, os_build }
}

/// Appends the real signal's POSIX name to `reason` when the raw exception is our synthetic
/// `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` shape (see `crate::snapshot::summary::signal_name`'s
/// doc comment): `minidump`'s own `get_crash_reason` has no idea what to do with it (Breakpad's
/// convention only special-cases one hard-coded `SIGABRT` encoding, which isn't the one we
/// produce), so every one of our software-only signals otherwise shows up as a bare/unknown
/// `EXC_SOFTWARE` with no indication of which real signal it was.
fn with_signal_name(reason: String, exc: &minidump_common::format::MINIDUMP_EXCEPTION) -> String {
    const EXC_SOFTWARE: u64 = 5;
    const EXC_SOFT_SIGNAL: u64 = 0x0001_0003;
    if exc.number_parameters < 3
        || exc.exception_information[0] != EXC_SOFTWARE
        || exc.exception_information[1] != EXC_SOFT_SIGNAL
    {
        return reason;
    }
    match crate::snapshot::summary::signal_name(exc.exception_information[2]) {
        Some(signal) if !reason.contains(signal) => format!("{reason} / {signal}"),
        _ => reason,
    }
}

/// A symbol-free walk of **every thread**: one `FrameRef` (image UUID + offset into it) per frame that lands in a
/// module. The offset is that of the *call instruction* for caller frames (`StackFrame::instruction`), which is
/// the address that names the function.
fn all_frames(dump: &minidump::Minidump<'_, &[u8]>) -> Vec<crate::syshints::FrameRef> {
    use crate::syshints::FrameRef;
    use minidump::{MinidumpModuleList, Module};
    use minidump_unwind::{CallStack, MultiSymbolProvider, SystemInfo, walk_stack};

    let (Ok(sys), Ok(threads)) =
        (dump.get_stream::<minidump::MinidumpSystemInfo>(), dump.get_stream::<minidump::MinidumpThreadList>())
    else {
        return Vec::new();
    };
    let modules = dump.get_stream::<MinidumpModuleList>().unwrap_or_else(|_| MinidumpModuleList::new());
    let misc = dump.get_stream::<minidump::MinidumpMiscInfo>().ok();
    let memory = dump.get_memory().unwrap_or_default();
    let unwind_sys = SystemInfo {
        os: sys.os,
        cpu: sys.cpu,
        os_version: None,
        os_build: None,
        cpu_info: None,
        cpu_microcode_version: None,
        cpu_count: 1,
    };
    let provider = MultiSymbolProvider::new();

    let mut out = Vec::new();
    for thread in &threads.threads {
        let Some(context) = thread.context(&sys, misc.as_ref()) else { continue };
        let tid = thread.raw.thread_id;
        let mut stack = CallStack::with_context(context.into_owned());
        stack.thread_id = tid;
        futures::executor::block_on(walk_stack(
            0,
            (),
            &mut stack,
            thread.stack_memory(&memory),
            &modules,
            &unwind_sys,
            &provider,
        ));
        for frame in &stack.frames {
            let Some(module) = &frame.module else { continue };
            let Some(id) = module.debug_identifier() else { continue };
            out.push(FrameRef {
                thread: u64::from(tid),
                uuid: *id.uuid().as_bytes(),
                offset: frame.instruction.wrapping_sub(module.raw.base_of_image),
                path: module.code_file().into_owned(),
            });
        }
    }
    out
}

/// A symbol-free walk of the crashing thread (`module +offset [trust]`
/// frames) followed by the module table (base · breakpad debug-id · file) —
/// enough to symbolicate offsets against the matching build's `.dSYM` / PDB /
/// DWARF. No process memory, so it's safe to e-mail.
fn stack_summary(dump: &minidump::Minidump<'_, &[u8]>) -> String {
    use minidump::{MinidumpModuleList, Module};
    use minidump_unwind::{CallStack, FrameTrust, MultiSymbolProvider, SystemInfo, walk_stack};
    use std::fmt::Write as _;

    let Ok(sys) = dump.get_stream::<minidump::MinidumpSystemInfo>() else {
        return String::new();
    };
    let modules = dump.get_stream::<MinidumpModuleList>().unwrap_or_else(|_| MinidumpModuleList::new());
    let threads = dump.get_stream::<minidump::MinidumpThreadList>().ok();
    let exc = dump.get_stream::<minidump::MinidumpException>().ok();
    let misc = dump.get_stream::<minidump::MinidumpMiscInfo>().ok();
    let memory = dump.get_memory().unwrap_or_default();
    let crashing_tid = exc.as_ref().map(|e| e.get_crashing_thread_id());

    let unwind_sys = SystemInfo {
        os: sys.os,
        cpu: sys.cpu,
        os_version: None,
        os_build: None,
        cpu_info: None,
        cpu_microcode_version: None,
        cpu_count: 1,
    };
    let provider = MultiSymbolProvider::new();
    let mut out = String::new();

    let base = |file: &str| file.rsplit(['/', '\\']).next().unwrap_or(file).to_owned();

    if let (Some(threads), Some(tid)) = (threads, crashing_tid)
        && let Some(thread) = threads.threads.iter().find(|t| t.raw.thread_id == tid)
        && let Some(context) = thread.context(&sys, misc.as_ref())
    {
        let mut stack = CallStack::with_context(context.into_owned());
        stack.thread_id = tid;
        let stack_memory = thread.stack_memory(&memory);
        futures::executor::block_on(walk_stack(0, (), &mut stack, stack_memory, &modules, &unwind_sys, &provider));

        let _ = writeln!(out, "Crashing thread [{tid:#x}]:");
        for (n, frame) in stack.frames.iter().enumerate() {
            let trust = match frame.trust {
                FrameTrust::Context => "ctx",
                FrameTrust::CfiScan | FrameTrust::Scan => "scan",
                FrameTrust::FramePointer => "fp",
                FrameTrust::CallFrameInfo => "cfi",
                FrameTrust::PreWalked => "pre",
                FrameTrust::None => "?",
            };
            let ip = frame.instruction;
            match &frame.module {
                Some(m) => {
                    let off = ip.wrapping_sub(m.raw.base_of_image);
                    let _ = writeln!(out, "  #{n:<3} {ip:#018x}  {} +{off:#x}  [{trust}]", base(&m.code_file()));
                }
                None => {
                    let _ = writeln!(out, "  #{n:<3} {ip:#018x}  ???  [{trust}]");
                }
            }
        }
    }

    if modules.iter().next().is_some() {
        let _ = writeln!(out, "\nModules:");
        for m in modules.iter() {
            let id = m.debug_identifier().map(|d| d.breakpad().to_string()).unwrap_or_else(|| "-".into());
            let _ = writeln!(out, "  {:#018x}  {:<40}  {}", m.raw.base_of_image, id, base(&m.code_file()));
        }
    }

    out
}

fn write_file(path: &str, bytes: &[u8]) -> bool {
    use std::io::Write;
    let Ok(mut f) = std::fs::File::create(path) else {
        return false;
    };
    let ok = f.write_all(bytes).is_ok();
    let _ = f.sync_all();
    ok
}
