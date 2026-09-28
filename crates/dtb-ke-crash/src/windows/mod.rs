//! Windows crash capture — the *thin* half.
//!
//! `install()` (clean context): create a named "dump captured" event, an
//! internal "crash started" event, resolve this process's own executable path
//! (for the relaunch) + a `"<exe>" --dtb-ke-crash-reporter` command line, spawn
//! a watchdog thread (no `alarm` on Windows → a thread that `TerminateProcess`es
//! us 12 s after the crash starts), and install a top-level
//! `SetUnhandledExceptionFilter`.
//!
//! On a fault the filter (in the faulting thread's context):
//! 1. signal the watchdog,
//! 2. `CreatePipe` + `CreateProcessW` **this same executable**, relaunched with
//!    [`crate::RELAUNCH_ARG`] on its command line — no embedded helper to
//!    extract any more, just the app's own binary again — with the pipe on
//!    stdin,
//! 3. write its params — dump dir, slug, the done-event name, our pid, the
//!    faulting tid, the `EXCEPTION_POINTERS` address, the exception code, the
//!    panic message — NUL-separated,
//! 4. `WaitForSingleObject` on the done event (the relaunched instance
//!    `SetEvent`s it the moment `MiniDumpWriteDump` returns), then
//!    `TerminateProcess(self, 134)`.
//!
//! `MiniDumpWriteDump` and everything analytical run in the relaunched instance
//! (`dtb-ke-ui`'s own `crash_report::run_windows_capture`, via
//! `dtb_ke_crash::writer::windows::capture` + `writer::build_report`) — it *is*
//! the crash reporter too, no further relaunch needed. `CreateProcessW` from a
//! crashed thread takes the loader lock — a fault *through* it is "something
//! else is already broken"; the watchdog bounds it either way.

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicBool, Ordering};

type Handle = *mut core::ffi::c_void;

const STARTF_USESTDHANDLES: u32 = 0x0000_0100;
const HANDLE_FLAG_INHERIT: u32 = 0x0000_0001;
const STD_OUTPUT_HANDLE: u32 = 0xFFFF_FFF5; // -11
const STD_ERROR_HANDLE: u32 = 0xFFFF_FFF4; // -12
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const INFINITE: u32 = 0xFFFF_FFFF;
const EXCEPTION_CONTINUE_SEARCH: i32 = 0;
const WATCHDOG_MS: u32 = 12_000;

#[repr(C)]
struct SecurityAttributes {
    n_length: u32,
    lp_security_descriptor: *mut core::ffi::c_void,
    b_inherit_handle: i32,
}

#[repr(C)]
struct StartupInfoW {
    cb: u32,
    lp_reserved: *mut u16,
    lp_desktop: *mut u16,
    lp_title: *mut u16,
    dw_x: u32,
    dw_y: u32,
    dw_x_size: u32,
    dw_y_size: u32,
    dw_x_count_chars: u32,
    dw_y_count_chars: u32,
    dw_fill_attribute: u32,
    dw_flags: u32,
    w_show_window: u16,
    cb_reserved2: u16,
    lp_reserved2: *mut u8,
    h_std_input: Handle,
    h_std_output: Handle,
    h_std_error: Handle,
}

#[repr(C)]
struct ProcessInformation {
    h_process: Handle,
    h_thread: Handle,
    dw_process_id: u32,
    dw_thread_id: u32,
}

#[repr(C)]
struct ExceptionRecord {
    exception_code: u32,
    exception_flags: u32,
    exception_record: *mut ExceptionRecord,
    exception_address: *mut core::ffi::c_void,
    number_parameters: u32,
    exception_information: [usize; 15],
}

#[repr(C)]
struct ExceptionPointers {
    exception_record: *mut ExceptionRecord,
    context_record: *mut core::ffi::c_void,
}

type TopLevelFilter = unsafe extern "system" fn(*mut ExceptionPointers) -> i32;

unsafe extern "system" {
    fn SetUnhandledExceptionFilter(filter: Option<TopLevelFilter>) -> Option<TopLevelFilter>;
    fn GetCurrentProcess() -> Handle;
    fn GetCurrentProcessId() -> u32;
    fn GetCurrentThreadId() -> u32;
    fn TerminateProcess(process: Handle, exit_code: u32) -> i32;
    fn CreateEventW(
        attrs: *const SecurityAttributes,
        manual: i32,
        initial: i32,
        name: *const u16,
    ) -> Handle;
    fn SetEvent(event: Handle) -> i32;
    fn WaitForSingleObject(handle: Handle, ms: u32) -> u32;
    fn WriteFile(
        file: Handle,
        buf: *const u8,
        len: u32,
        written: *mut u32,
        overlapped: *mut core::ffi::c_void,
    ) -> i32;
    fn CloseHandle(handle: Handle) -> i32;
    fn CreatePipe(
        read: *mut Handle,
        write: *mut Handle,
        attrs: *const SecurityAttributes,
        size: u32,
    ) -> i32;
    fn SetHandleInformation(object: Handle, mask: u32, flags: u32) -> i32;
    fn GetStdHandle(which: u32) -> Handle;
    fn CreateProcessW(
        app: *const u16,
        cmdline: *mut u16,
        proc_attrs: *const SecurityAttributes,
        thread_attrs: *const SecurityAttributes,
        inherit: i32,
        flags: u32,
        env: *mut core::ffi::c_void,
        cwd: *const u16,
        startup: *const StartupInfoW,
        info: *mut ProcessInformation,
    ) -> i32;
    fn Sleep(ms: u32);
}

// ── module state, all set in install() ─────────────────────────────────

static mut DUMP_DIR: [u8; 1024] = [0; 1024];
static mut APP_SLUG: [u8; 64] = [0; 64];
static mut DONE_EVENT_NAME: [u8; 128] = [0; 128];
/// This process's own executable path, UTF-16, NUL-terminated — the relaunch target
/// (`CreateProcessW`'s `lpApplicationName`).
static mut MAIN_EXE_W: [u16; 1024] = [0; 1024];
/// `"<MAIN_EXE_W>" --dtb-ke-crash-reporter`, UTF-16, NUL-terminated — the relaunch command line
/// (`CreateProcessW`'s `lpCommandLine`, which it may modify in place, hence a fresh mutable copy of
/// this static at each use).
static mut CMDLINE_W: [u16; 1024] = [0; 1024];
static mut DONE_EVENT: Handle = std::ptr::null_mut();
static mut WATCHDOG_START: Handle = std::ptr::null_mut();
static HANDLED: AtomicBool = AtomicBool::new(false);

// ── install ────────────────────────────────────────────────────────────

pub(crate) fn install(
    dump_dir: &std::path::Path,
    app_slug: &str,
) -> Result<(), crate::library::InstallError> {
    unsafe {
        put_cstr(&mut DUMP_DIR, dump_dir.to_string_lossy().as_bytes());
        put_cstr(&mut APP_SLUG, app_slug.as_bytes());

        let pid = GetCurrentProcessId();
        let ev_name = format!("Local\\dtb-ke-crash-{pid}-done");
        put_cstr(&mut DONE_EVENT_NAME, ev_name.as_bytes());
        DONE_EVENT = CreateEventW(std::ptr::null(), 1, 0, wide(&ev_name).as_ptr());
        WATCHDOG_START = CreateEventW(std::ptr::null(), 0, 0, std::ptr::null());
        if DONE_EVENT.is_null() || WATCHDOG_START.is_null() {
            return Err(crate::library::InstallError::Os(
                "CreateEventW failed".into(),
            ));
        }

        if let Ok(exe) = std::env::current_exe() {
            let exe_w: Vec<u16> = exe.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
            put_wide(&mut MAIN_EXE_W, &exe_w);

            // `"<exe>" --dtb-ke-crash-reporter` — quoted since the path may contain spaces.
            let mut cmd: Vec<u16> = Vec::new();
            cmd.push(b'"' as u16);
            cmd.extend(exe.as_os_str().encode_wide());
            cmd.push(b'"' as u16);
            cmd.push(b' ' as u16);
            cmd.extend(crate::RELAUNCH_ARG.encode_utf16());
            cmd.push(0);
            put_wide(&mut CMDLINE_W, &cmd);
        }

        std::thread::Builder::new()
            .name("dtbke-crash-watchdog".into())
            .stack_size(64 * 1024)
            .spawn(|| unsafe {
                WaitForSingleObject(WATCHDOG_START, INFINITE);
                Sleep(WATCHDOG_MS);
                TerminateProcess(GetCurrentProcess(), 134);
            })
            .map_err(|e| crate::library::InstallError::Os(e.to_string()))?;

        SetUnhandledExceptionFilter(Some(filter));
    }
    Ok(())
}

unsafe fn put_cstr(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
}

unsafe fn put_wide(dst: &mut [u16], src: &[u16]) {
    let n = src.len().min(dst.len());
    dst[..n].copy_from_slice(&src[..n]);
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

// ── the exception filter ───────────────────────────────────────────────

unsafe extern "system" fn filter(ep: *mut ExceptionPointers) -> i32 {
    if HANDLED.swap(true, Ordering::SeqCst) {
        TerminateProcess(GetCurrentProcess(), 134);
    }
    SetEvent(WATCHDOG_START);

    let exc_code = if ep.is_null() || (*ep).exception_record.is_null() {
        0u32
    } else {
        (*(*ep).exception_record).exception_code
    };
    let pid = GetCurrentProcessId();
    let tid = GetCurrentThreadId();

    dtb_ke_log::fault!("------------[ cut here ]------------");
    crate::oops_art();
    if crate::panic::panicking() {
        dtb_ke_log::fault!("Rust panic, trapping to an out-of-process minidump");
    } else {
        dtb_ke_log::fault!(
            "Unhandled exception {exc_code:#010x} on thread {tid}, capturing a minidump"
        );
    }

    // Pipe for the relaunched instance's stdin — read end inheritable, write end not.
    let sa = SecurityAttributes {
        n_length: size_of::<SecurityAttributes>() as u32,
        lp_security_descriptor: std::ptr::null_mut(),
        b_inherit_handle: 1,
    };
    let mut rd: Handle = std::ptr::null_mut();
    let mut wr: Handle = std::ptr::null_mut();
    if CreatePipe(&mut rd, &mut wr, &sa, 0) == 0 {
        dtb_ke_log::fault!("Could not open the capture pipe, no minidump");
        dtb_ke_log::fault!(" ---[ end trace {exc_code:#x} ]---");
        TerminateProcess(GetCurrentProcess(), 134);
    }
    SetHandleInformation(wr, HANDLE_FLAG_INHERIT, 0);

    let mut si: StartupInfoW = std::mem::zeroed();
    si.cb = size_of::<StartupInfoW>() as u32;
    si.dw_flags = STARTF_USESTDHANDLES;
    si.h_std_input = rd;
    si.h_std_output = GetStdHandle(STD_OUTPUT_HANDLE);
    si.h_std_error = GetStdHandle(STD_ERROR_HANDLE);
    let mut pi: ProcessInformation = std::mem::zeroed();

    let mut cmdline = CMDLINE_W; // mutable copy for CreateProcessW
    let ok = CreateProcessW(
        MAIN_EXE_W.as_ptr(),
        cmdline.as_mut_ptr(),
        std::ptr::null(),
        std::ptr::null(),
        1,
        CREATE_NO_WINDOW,
        std::ptr::null_mut(),
        std::ptr::null(),
        &si,
        &mut pi,
    );
    CloseHandle(rd);
    if ok == 0 {
        dtb_ke_log::fault!("Could not relaunch for capture, no minidump");
        CloseHandle(wr);
        dtb_ke_log::fault!(" ---[ end trace {exc_code:#x} ]---");
        TerminateProcess(GetCurrentProcess(), 134);
    }
    CloseHandle(pi.h_thread);
    CloseHandle(pi.h_process);

    // params: dump_dir \0 slug \0 done_event \0 pid \0 tid \0
    //         exc_ptr \0 exc_code \0 panic_msg \0 build_info
    let w = PipeW(wr);
    w.cstr(&DUMP_DIR);
    w.sep();
    w.cstr(&APP_SLUG);
    w.sep();
    w.cstr(&DONE_EVENT_NAME);
    w.sep();
    w.u64(pid as u64);
    w.sep();
    w.u64(tid as u64);
    w.sep();
    w.u64(ep as usize as u64);
    w.sep();
    w.u64(exc_code as u64);
    w.sep();
    w.cstr_ptr(crate::panic::message_ptr());
    w.sep();
    w.cstr_ptr(crate::buildinfo::cstr_ptr());
    CloseHandle(wr);

    dtb_ke_log::fault!(" ---[ end trace {exc_code:#x} ]---");
    WaitForSingleObject(DONE_EVENT, WATCHDOG_MS);
    TerminateProcess(GetCurrentProcess(), 134);
    EXCEPTION_CONTINUE_SEARCH
}

/// Allocation-free writer over the relaunched instance's stdin pipe.
struct PipeW(Handle);

impl PipeW {
    fn raw(&self, b: &[u8]) {
        let mut off = 0usize;
        while off < b.len() {
            let mut written = 0u32;
            let n = (b.len() - off) as u32;
            if unsafe {
                WriteFile(
                    self.0,
                    b.as_ptr().add(off),
                    n,
                    &mut written,
                    std::ptr::null_mut(),
                )
            } == 0
                || written == 0
            {
                break;
            }
            off += written as usize;
        }
    }
    fn cstr(&self, b: &[u8]) {
        let n = b.iter().position(|&c| c == 0).unwrap_or(b.len());
        self.raw(&b[..n]);
    }
    unsafe fn cstr_ptr(&self, p: *const u8) {
        let mut n = 0usize;
        while *p.add(n) != 0 {
            n += 1;
        }
        self.raw(core::slice::from_raw_parts(p, n));
    }
    fn sep(&self) {
        self.raw(&[0]);
    }
    fn u64(&self, mut v: u64) {
        let mut buf = [0u8; 20];
        let mut i = buf.len();
        loop {
            i -= 1;
            buf[i] = b'0' + (v % 10) as u8;
            v /= 10;
            if v == 0 {
                break;
            }
        }
        self.raw(&buf[i..]);
    }
}
