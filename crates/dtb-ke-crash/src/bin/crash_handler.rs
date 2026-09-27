//! The out-of-process crash-dump worker.
//!
//! Spawned by `dtb-ke-crash` on a fault. It receives the crashed process's
//! rights / exception context (Mach ports on macOS, a pid + `EXCEPTION_POINTERS`
//! address on Windows), hands them to `minidump-writer` — which reads the crashed
//! process's memory and produces a standard **minidump** (`.dtbkedmp`) — then relaunches
//! the app as a crash reporter (recognised via the parent process), streams it a
//! digest + the dump, waits for a `report::*` verdict, and keeps or deletes the
//! `.dtbkedmp`. The crashed process never runs a line of our code in a fragile state.

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
fn main() {
    std::process::exit(0);
}

#[cfg(target_os = "macos")]
fn main() {
    macos::run();
}

#[cfg(target_os = "windows")]
fn main() {
    windows::run();
}

#[cfg(target_os = "linux")]
fn main() {
    linux::run();
}

// ── shared: write the dump, run the reporter, act on its verdict ────────────

#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
mod common {
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Persist the dump, then relaunch `main_exe` as the crash reporter, stream it
    /// the digest + dump, and delete the `.dtbkedmp` iff the reporter exited `SENT`.
    #[allow(clippy::too_many_arguments)] // one positional arg per wire field, 1:1 with the pipe layout
    pub fn finish(
        dump_dir: &str,
        slug: &str,
        pid: i64,
        mut dmp: Vec<u8>,
        main_exe: &str,
        panic_msg: &str,
        build_info: &str,
        nsexception: &str,
    ) {
        // The build-info user stream, so a debugger can tell exactly which build crashed. A failure
        // here must never cost us the dump itself.
        if !build_info.is_empty() {
            let _ = dtb_ke_crash::patch::append_stream(
                &mut dmp,
                dtb_ke_crash::buildinfo::STREAM_TYPE,
                build_info.as_bytes(),
            );
        }
        // The uncaught-`NSException` user stream (macOS only, empty for every other fault kind —
        // see `dtb_ke_crash::nsexception`).
        if !nsexception.is_empty() {
            let _ = dtb_ke_crash::patch::append_stream(
                &mut dmp,
                dtb_ke_crash::nsexception::STREAM_TYPE,
                nsexception.as_bytes(),
            );
        }

        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = format!("{dump_dir}/{slug}-{secs}-{pid}.{}", dtb_ke_crash::DUMP_EXTENSION);

        // Persist first — we are the reliable actor; the reporter may never come
        // up (headless, gpui failure).
        let persisted = write_file(&path, &dmp);

        if main_exe.is_empty() {
            return;
        }
        let exe = std::fs::canonicalize(main_exe)
            .map(|c| c.to_string_lossy().into_owned())
            .unwrap_or_else(|_| main_exe.to_string());
        let framed = frame_digest(&dmp, panic_msg, &path);
        if reporter_says_sent(&exe, framed) && persisted {
            let _ = std::fs::remove_file(&path);
        }
    }

    /// `reason \0 address \0 thread \0 panic_msg \0 stack \0 frames \0 dump_path \0 os_build \0 <minidump>` — the
    /// reporter's digest. `thread` is `«name» (0x…)` when the dump names it, else
    /// `0x…`; `stack` is a symbol-free walk of the crashing thread + the module
    /// table (for offset-wise symbolication). The helper parses the `.dtbkedmp` so
    /// `dtb-ke-ui` needn't link `minidump`.
    fn frame_digest(dmp: &[u8], panic_msg: &str, dump_path: &str) -> Vec<u8> {
        let parsed = minidump::Minidump::read(dmp).ok();
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
        let stack = parsed
            .as_ref()
            .map(|dump| stack_summary(dump))
            .unwrap_or_default();
        // Every thread's frames as (image UUID, offset) for the reporter to name system-library frames with, and
        // the OS build those names belong to.
        let frames = parsed
            .as_ref()
            .map(|dump| dtb_ke_crash::syshints::encode_frames(&all_frames(dump)))
            .unwrap_or_default();
        let os_build = parsed
            .as_ref()
            .and_then(|dump| dump.get_stream::<minidump::MinidumpSystemInfo>().ok())
            .and_then(|sys| sys.csd_version().map(|b| b.trim().to_owned()))
            .filter(|b| b.len() <= 12 && !b.contains(' '))
            .unwrap_or_default();

        let mut v = Vec::with_capacity(dmp.len() + stack.len() + 256);
        for field in [
            reason.as_str(),
            address.as_str(),
            thread.as_str(),
            panic_msg,
            stack.as_str(),
            frames.as_str(),
            dump_path,
            os_build.as_str(),
        ] {
            v.extend_from_slice(field.as_bytes());
            v.push(0);
        }
        v.extend_from_slice(dmp);
        v
    }

    /// Appends the real signal's POSIX name to `reason` when the raw exception is our synthetic
    /// `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` shape (see `dtb_ke_crash::snapshot::summary::signal_name`'s
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
        match dtb_ke_crash::snapshot::summary::signal_name(exc.exception_information[2]) {
            Some(signal) if !reason.contains(signal) => format!("{reason} / {signal}"),
            _ => reason,
        }
    }

    /// A symbol-free walk of **every thread**: one `FrameRef` (image UUID + offset into it) per frame that lands in a
    /// module. The offset is that of the *call instruction* for caller frames (`StackFrame::instruction`), which is
    /// the address that names the function.
    fn all_frames(dump: &minidump::Minidump<'_, &[u8]>) -> Vec<dtb_ke_crash::syshints::FrameRef> {
        use dtb_ke_crash::syshints::FrameRef;
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
        let modules = dump
            .get_stream::<MinidumpModuleList>()
            .unwrap_or_else(|_| MinidumpModuleList::new());
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
            futures::executor::block_on(walk_stack(
                0,
                (),
                &mut stack,
                stack_memory,
                &modules,
                &unwind_sys,
                &provider,
            ));

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
                        let _ = writeln!(
                            out,
                            "  #{n:<3} {ip:#018x}  {} +{off:#x}  [{trust}]",
                            base(&m.code_file())
                        );
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
                let id = m
                    .debug_identifier()
                    .map(|d| d.breakpad().to_string())
                    .unwrap_or_else(|| "-".into());
                let _ = writeln!(
                    out,
                    "  {:#018x}  {:<40}  {}",
                    m.raw.base_of_image,
                    id,
                    base(&m.code_file())
                );
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

    fn reporter_says_sent(exe: &str, framed: Vec<u8>) -> bool {
        use dtb_ke_crash::report;
        use std::io::Write;
        use std::process::{Command, Stdio};

        let Ok(mut child) = Command::new(exe).stdin(Stdio::piped()).spawn() else {
            return false;
        };
        // Larger than a pipe buffer — feed it from another thread so a blocking
        // `write_all` before `wait()` can't deadlock.
        if let Some(mut stdin) = child.stdin.take() {
            std::thread::spawn(move || {
                let _ = stdin.write_all(&framed);
            });
        }
        matches!(child.wait(), Ok(status) if status.code() == Some(report::SENT))
    }
}

#[cfg(target_os = "macos")]
mod macos {
    #![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn)]

    use std::io::Cursor;
    use std::mem::{size_of, zeroed};

    use crash_context::{CrashContext, ExceptionInfo};
    use mach2::bootstrap::{bootstrap_look_up, bootstrap_port};
    use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
    use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
    use mach2::message::{
        MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
        MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX, MACH_RCV_MSG, MACH_SEND_MSG, mach_msg,
        mach_msg_body_t, mach_msg_header_t, mach_msg_port_descriptor_t,
    };
    use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
    use minidump_writer::minidump_writer::MinidumpWriter;

    const MSG_ID_HELPER_HELLO: i32 = 0x6b63_0001;
    const MSG_ID_FORWARD: i32 = 0x6b63_0002;
    const MSG_ID_HELPER_DONE: i32 = 0x6b63_0003;

    unsafe extern "C" {
        fn pid_for_task(task: mach_port_t, pid: *mut i32) -> kern_return_t;
        fn task_suspend(task: mach_port_t) -> kern_return_t;
        fn task_resume(task: mach_port_t) -> kern_return_t;
    }

    pub fn run() {
        // Parameters arrive on stdin as NUL-separated fields (see `dtb-ke-crash`):
        // dump_dir, slug, bootstrap_name, main_exe, panic_msg (empty for a
        // hardware fault), build_info, nsexception (empty unless an uncaught
        // `NSException` is what's crashing us).
        use std::io::Read;
        let mut blob = Vec::with_capacity(1024);
        if std::io::stdin().read_to_end(&mut blob).is_err() || blob.is_empty() {
            std::process::exit(2);
        }
        let mut fields = blob.split(|&b| b == 0);
        let mut next = || {
            fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .unwrap_or_default()
        };
        let dump_dir = next();
        let slug = next();
        let bootstrap_name = next();
        let main_exe = next();
        let panic_msg = next();
        let build_info = next();
        let nsexception = next();
        if dump_dir.is_empty() || bootstrap_name.is_empty() {
            std::process::exit(2);
        }

        let Some((dmp, pid)) = (unsafe { capture(&bootstrap_name) }) else {
            std::process::exit(3)
        };

        super::common::finish(
            &dump_dir,
            &slug,
            pid as i64,
            dmp,
            &main_exe,
            &panic_msg,
            &build_info,
            &nsexception,
        );
    }

    unsafe fn capture(bootstrap_name: &str) -> Option<(Vec<u8>, i32)> {
        // ── handshake ──
        let cname = std::ffi::CString::new(bootstrap_name).ok()?;
        let mut s_send: mach_port_t = MACH_PORT_NULL;
        // The parent registers the service well before any crash, and we inherit
        // its bootstrap namespace across fork/exec — but retry in case the lookup
        // races the registration's propagation.
        let mut tries = 0;
        while bootstrap_look_up(bootstrap_port, cname.as_ptr(), &mut s_send) != KERN_SUCCESS {
            tries += 1;
            if tries >= 20 {
                return None;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }

        let me = mach2::traps::mach_task_self();
        let mut r: mach_port_t = MACH_PORT_NULL;
        if mach_port_allocate(me, MACH_PORT_RIGHT_RECEIVE, &mut r) != KERN_SUCCESS {
            return None;
        }
        mach_port_insert_right(me, r, r, MACH_MSG_TYPE_MAKE_SEND);

        // HELLO: send R to S.
        #[repr(C)]
        struct Hello {
            head: mach_msg_header_t,
            body: mach_msg_body_t,
            port: mach_msg_port_descriptor_t,
        }
        let mut hello: Hello = zeroed();
        hello.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
        hello.head.msgh_size = size_of::<Hello>() as u32;
        hello.head.msgh_remote_port = s_send;
        hello.head.msgh_id = MSG_ID_HELPER_HELLO;
        hello.body.msgh_descriptor_count = 1;
        hello.port = mach_msg_port_descriptor_t::new(r, MACH_MSG_TYPE_MAKE_SEND);
        if mach_msg(
            &mut hello.head,
            MACH_SEND_MSG,
            size_of::<Hello>() as u32,
            0,
            MACH_PORT_NULL,
            MACH_MSG_TIMEOUT_NONE,
            MACH_PORT_NULL,
        ) != MACH_MSG_SUCCESS
        {
            return None;
        }

        // FORWARD: receive task + faulting thread + handler thread + exception
        // triple on R. Read back through the *exact same* `#[repr(C)]` struct the
        // sender lays out (`dtb_ke_crash`'s `handle`'s `Forward`) — keep in sync.
        #[repr(C)]
        struct Forward {
            head: mach_msg_header_t,
            body: mach_msg_body_t,
            task: mach_msg_port_descriptor_t,
            thread: mach_msg_port_descriptor_t,
            handler_thread: mach_msg_port_descriptor_t,
            exception: u32,
            _pad: u32,
            code: u64,
            subcode: u64,
        }
        #[repr(C, align(8))]
        struct Buf([u8; 512]);
        let mut buf = Buf([0; 512]);
        if mach_msg(
            buf.0.as_mut_ptr() as *mut mach_msg_header_t,
            MACH_RCV_MSG,
            0,
            buf.0.len() as u32,
            r,
            MACH_MSG_TIMEOUT_NONE,
            MACH_PORT_NULL,
        ) != MACH_MSG_SUCCESS
        {
            return None;
        }
        if buf.0.len() < size_of::<Forward>() {
            return None;
        }
        let fwd = &*(buf.0.as_ptr() as *const Forward);
        if fwd.head.msgh_id != MSG_ID_FORWARD {
            return None;
        }
        // The kernel rewrites the descriptor `.name`s to our local port names.
        let task: mach_port_t = fwd.task.name;
        let thread: mach_port_t = fwd.thread.name;
        let handler_thread: mach_port_t = fwd.handler_thread.name;
        let exception = fwd.exception;
        let code = fwd.code;
        let subcode = fwd.subcode;

        let mut pid = 0i32;
        pid_for_task(task, &mut pid);

        // ── dump ── freeze the task for a consistent snapshot, then let
        // `minidump-writer` walk it via the forwarded ports.
        task_suspend(task);

        let cc = CrashContext {
            task,
            thread,
            handler_thread,
            exception: Some(ExceptionInfo {
                kind: exception,
                code,
                subcode: Some(subcode),
            }),
        };
        let mut cur = Cursor::new(Vec::<u8>::new());
        let dumped = MinidumpWriter::with_crash_context(cc).dump(&mut cur);

        task_resume(task);

        // ── done: the parent may exit now (it was blocked, hence suspended too;
        // the resume above lets it wake to receive this). ──
        #[repr(C)]
        struct Done {
            head: mach_msg_header_t,
            status: u32,
        }
        let mut done: Done = zeroed();
        done.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
        done.head.msgh_size = size_of::<Done>() as u32;
        done.head.msgh_remote_port = s_send;
        done.head.msgh_id = MSG_ID_HELPER_DONE;
        let _ = mach_msg(
            &mut done.head,
            MACH_SEND_MSG,
            size_of::<Done>() as u32,
            0,
            MACH_PORT_NULL,
            MACH_MSG_TIMEOUT_NONE,
            MACH_PORT_NULL,
        );

        let bytes = dumped.ok()?;
        (!bytes.is_empty()).then_some((bytes, pid))
    }
}

#[cfg(target_os = "windows")]
mod windows {
    #![allow(clippy::missing_safety_doc)]

    use std::os::windows::ffi::OsStrExt;

    use crash_context::CrashContext;
    use minidump_writer::minidump_writer::MinidumpWriter;

    type Handle = *mut core::ffi::c_void;
    const EVENT_MODIFY_STATE: u32 = 0x0002;

    unsafe extern "system" {
        fn OpenEventW(access: u32, inherit: i32, name: *const u16) -> Handle;
        fn SetEvent(h: Handle) -> i32;
        fn CloseHandle(h: Handle) -> i32;
    }

    fn wide(s: &str) -> Vec<u16> {
        std::ffi::OsStr::new(s)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect()
    }

    pub fn run() {
        use std::io::Read;
        let mut blob = Vec::with_capacity(1024);
        if std::io::stdin().read_to_end(&mut blob).is_err() || blob.is_empty() {
            std::process::exit(2);
        }
        let mut fields = blob.split(|&b| b == 0);
        let mut next = || {
            fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .unwrap_or_default()
        };
        // dump_dir, slug, main_exe, done_event, pid, tid, exc_ptr, exc_code, panic_msg, build_info
        let dump_dir = next();
        let slug = next();
        let main_exe = next();
        let done_event = next();
        let pid: u32 = next().parse().unwrap_or(0);
        let tid: u32 = next().parse().unwrap_or(0);
        let exc_ptr: usize = next().parse().unwrap_or(0);
        let exc_code: i32 = next().parse().unwrap_or(0);
        let panic_msg = next();
        let build_info = next();
        if dump_dir.is_empty() || pid == 0 {
            std::process::exit(2);
        }

        // The named event the crashed process's exception filter is blocked on —
        // `SetEvent` releases it the moment the dump is captured.
        let done: Handle = if done_event.is_empty() {
            std::ptr::null_mut()
        } else {
            unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, wide(&done_event).as_ptr()) }
        };
        let release = || {
            if !done.is_null() {
                unsafe {
                    SetEvent(done);
                    CloseHandle(done);
                }
            }
        };

        let Some(dmp) = capture(pid, tid, exc_ptr, exc_code, &slug) else {
            release();
            std::process::exit(3);
        };
        release();

        // No `NSException` on Windows — the pipe carries no field for it.
        super::common::finish(&dump_dir, &slug, pid as i64, dmp, &main_exe, &panic_msg, &build_info, "");
    }

    fn capture(pid: u32, tid: u32, exc_ptr: usize, exc_code: i32, slug: &str) -> Option<Vec<u8>> {
        // `dump_crash_context` writes to a `File`; dump to a temp, read it back.
        let tmp = std::env::temp_dir().join(format!("{slug}-{pid}.{}.part", dtb_ke_crash::DUMP_EXTENSION));
        let mut file = std::fs::File::create(&tmp).ok()?;
        let cc = CrashContext {
            exception_pointers: exc_ptr as *const _,
            exception_code: exc_code,
            process_id: pid,
            thread_id: tid,
        };
        let res = MinidumpWriter::dump_crash_context(&cc, None, &mut file);
        let _ = file.sync_all();
        drop(file);
        let bytes = std::fs::read(&tmp).ok();
        let _ = std::fs::remove_file(&tmp);
        res.ok()?;
        bytes.filter(|b| !b.is_empty())
    }
}

#[cfg(target_os = "linux")]
mod linux {
    #![allow(clippy::missing_safety_doc)]

    use std::io::{Cursor, Read};

    use minidump_writer::CrashContextExt;
    use minidump_writer::minidump_writer::MinidumpWriterConfig;

    pub fn run() {
        // Parameters arrive on stdin, NUL-separated (see `dtb-ke-crash`'s Linux
        // handler): dump_dir, slug, main_exe, pid, tid, signo, si_code, si_addr,
        // done_fd, panic_msg, build_info, then the raw `ucontext_t` bytes as the tail.
        let mut blob = Vec::with_capacity(8192);
        if std::io::stdin().read_to_end(&mut blob).is_err() || blob.is_empty() {
            std::process::exit(2);
        }
        let mut fields = blob.splitn(12, |&b| b == 0);
        let mut txt = || {
            fields
                .next()
                .map(|f| String::from_utf8_lossy(f).into_owned())
                .unwrap_or_default()
        };
        let dump_dir = txt();
        let slug = txt();
        let main_exe = txt();
        let pid: i32 = txt().parse().unwrap_or(0);
        let tid: i32 = txt().parse().unwrap_or(0);
        let signo: u32 = txt().parse().unwrap_or(0);
        let si_code: i32 = txt().parse().unwrap_or(0);
        let si_addr: u64 = txt().parse().unwrap_or(0);
        let done_fd: i32 = txt().parse().unwrap_or(-1);
        let panic_msg = txt();
        let build_info = txt();
        let uctx = fields.next().unwrap_or(&[]).to_vec();

        // Release the crashed process the moment we're done reading it — it is
        // blocked on `read(done_fd)`. Do this on every exit path.
        let release = || {
            if done_fd >= 0 {
                let one = [1u8];
                unsafe { libc::write(done_fd, one.as_ptr().cast(), 1) };
            }
        };

        if dump_dir.is_empty() || pid == 0 {
            release();
            std::process::exit(2);
        }

        let dmp = capture(pid, tid, signo, si_code, si_addr, &uctx);
        release();

        let Some(dmp) = dmp else {
            std::process::exit(3)
        };
        // No `NSException` on Linux — the pipe carries no field for it.
        super::common::finish(&dump_dir, &slug, pid as i64, dmp, &main_exe, &panic_msg, &build_info, "");
    }

    fn capture(
        pid: i32,
        tid: i32,
        signo: u32,
        si_code: i32,
        si_addr: u64,
        uctx: &[u8],
    ) -> Option<Vec<u8>> {
        let blamed = if tid != 0 { tid } else { pid };

        // Rebuild the signal `CrashContext`. `minidump-writer` reads the integer
        // registers from `context.uc_mcontext` (front of the struct, identical
        // ABI on both libc flavours) and the exception fields from `siginfo`;
        // `float_state` is not forwarded.
        let mut cc: crash_context::CrashContext = unsafe { std::mem::zeroed() };
        let n = uctx
            .len()
            .min(std::mem::size_of::<crash_context::ucontext_t>());
        unsafe {
            std::ptr::copy_nonoverlapping(
                uctx.as_ptr(),
                std::ptr::from_mut(&mut cc.context).cast::<u8>(),
                n,
            );
        }
        cc.siginfo.ssi_signo = signo;
        cc.siginfo.ssi_code = si_code;
        cc.siginfo.ssi_addr = si_addr;
        cc.pid = pid;
        cc.tid = blamed;

        let mut cfg = MinidumpWriterConfig::new(pid, blamed);
        cfg.set_crash_context(CrashContextExt { inner: cc });
        let mut cur = Cursor::new(Vec::<u8>::new());
        let bytes = cfg.write(&mut cur).ok()?;
        (!bytes.is_empty()).then_some(bytes)
    }
}
