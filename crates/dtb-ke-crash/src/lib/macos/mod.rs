//! macOS crash capture — the *thin* handler.
//!
//! `install()` (clean context): allocate a receive port `S`, register it in the
//! bootstrap namespace under a random name so a `fork`/`exec`'d child can look it
//! up, point the task exception ports at a second port `E`, and spawn a thread
//! blocked on `E`.
//!
//! On a fault the handler thread (syscalls + `mach_msg` only — no `malloc`, no
//! `_dyld_*`):
//! 1. extract the embedded helper to `$TMPDIR` (`open`/`write`/`fchmod`),
//! 2. `pipe` + `fork`; the child gets the pipe on stdin and `execv`s the helper
//!    with a bare argv (argv[0] only — nothing shows in `ps`),
//! 3. we write the helper's parameters (dump dir, slug, bootstrap name, crash
//!    kind, panic message) down the pipe as NUL-separated fields and close it,
//! 4. the child looks up `S`, sends back a reply port `R`,
//! 5. we forward the crashed **task + thread send rights** (moved straight out of
//!    the exception message) plus the exception info to `R`,
//! 6. we wait (bounded) for the helper's HELPER_DONE — sent the moment its
//!    `mach_vm_read`s are done — then terminate at once (`die()`, a raw `svc`
//!    exit — never `libc::_exit`, whose lazy stub deadlocks in `dyld_stub_binder`
//!    against a thread holding dyld's lock), so the crashed process and its
//!    frozen windows don't linger. Both mach receives are time-bounded so a
//!    helper that never starts can't wedge us; a 12 s `SIGALRM` watchdog
//!    (`on_sigalrm` → `die()`) is the absolute backstop.
//!
//! Everything analytical — thread state, stacks, the image list, building and
//! writing the dump — runs in the helper against the crashed task's memory. The
//! helper (now an orphan) then relaunches the *main binary* as a minimal crash
//! reporter, streams it the dump, and deletes the file only if the user's
//! choice comes back as "sent".

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::ffi::CString;
use std::mem::{size_of, zeroed};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use mach2::bootstrap::{
    BOOTSTRAP_SUCCESS, BOOTSTRAP_UNKNOWN_SERVICE, bootstrap_check_in, bootstrap_port,
};
use mach2::exception_types::{
    EXC_MASK_ARITHMETIC, EXC_MASK_BAD_ACCESS, EXC_MASK_BAD_INSTRUCTION, EXC_MASK_BREAKPOINT,
    EXCEPTION_DEFAULT, MACH_EXCEPTION_CODES, exception_behavior_t, exception_mask_t,
};
use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
use mach2::message::{
    MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
    MACH_MSG_TYPE_MOVE_SEND, MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX, MACH_RCV_MSG,
    MACH_RCV_TIMEOUT, MACH_SEND_MSG, mach_msg, mach_msg_body_t, mach_msg_header_t,
    mach_msg_port_descriptor_t,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
use mach2::thread_status::thread_state_flavor_t;
use mach2::traps::mach_task_self;

/// MIG id of the `mach_exception_raise` request.
const MSG_ID_EXCEPTION_RAISE: i32 = 2405;
/// Our own message ids (parent ⇄ helper).
const MSG_ID_HELPER_HELLO: i32 = 0x6b63_0001; // helper → S: "here is my reply port"
const MSG_ID_FORWARD: i32 = 0x6b63_0002; // parent → R: task + thread + exc info
const MSG_ID_HELPER_DONE: i32 = 0x6b63_0003; // helper → S: "I have read the task"

const EXC_MASK: exception_mask_t =
    EXC_MASK_BAD_ACCESS | EXC_MASK_BAD_INSTRUCTION | EXC_MASK_ARITHMETIC | EXC_MASK_BREAKPOINT;

/// Absolute backstop: if the helper handoff wedges (helper failed to start /
/// connect and every fallback below still somehow blocked), `SIGALRM` fires and
/// `on_sigalrm` `_exit`s cleanly — never a "terminated by SIGALRM".
const WATCHDOG_SECS: libc::c_uint = 12;
/// Per-`mach_msg`-receive timeout while waiting on the helper (ms).
const RECV_TIMEOUT_MS: libc::c_uint = 5000;

static HELPER_BYTES: &[u8] = include_bytes!(env!("DTB_KE_CRASH_HELPER"));

// ── module state, all set in install() ─────────────────────────────────

static mut RECV_PORT: mach_port_t = MACH_PORT_NULL; // S
static mut EXC_PORT: mach_port_t = MACH_PORT_NULL; // E
/// NUL-terminated, built once. `PATH_MAX`-bounded so the crash path never allocs.
static mut BOOTSTRAP_NAME: [u8; 128] = [0; 128];
static mut HELPER_PATH: [u8; 1024] = [0; 1024];
static mut DUMP_DIR: [u8; 1024] = [0; 1024];
static mut APP_SLUG: [u8; 64] = [0; 64];
/// The main executable's path — `minidump-writer` doesn't record it for the
/// executable module on macOS, so we resolve it at install and pipe it through
/// for the reporter relaunch.
static mut MAIN_EXE: [u8; 1024] = [0; 1024];
static HANDLED: AtomicBool = AtomicBool::new(false);

// ── hand-declared FFI ──────────────────────────────────────────────────

unsafe extern "C" {
    fn task_set_exception_ports(
        task: mach_port_t,
        exception_mask: exception_mask_t,
        new_port: mach_port_t,
        behavior: exception_behavior_t,
        new_flavor: thread_state_flavor_t,
    ) -> kern_return_t;
    fn bootstrap_create_service(
        bp: mach_port_t,
        service_name: *const libc::c_char,
        sp: *mut mach_port_t,
    ) -> kern_return_t;
}

// ── install ────────────────────────────────────────────────────────────

pub(crate) fn install(dump_dir: &Path, app_slug: &str) -> Result<(), crate::library::InstallError> {
    if HELPER_BYTES.is_empty() {
        return Err(crate::library::InstallError::NoHelper);
    }

    unsafe {
        put_cstr(&mut DUMP_DIR, dump_dir.to_string_lossy().as_bytes());
        put_cstr(&mut APP_SLUG, app_slug.as_bytes());

        // A per-process bootstrap name the child can look up.
        let name = format!(
            "de.philippremy.DTB-Kampfrichtereinsatzpläne.crashport.{}",
            std::process::id()
        );
        put_cstr(&mut BOOTSTRAP_NAME, name.as_bytes());

        // The helper temp path — resolved now, written to at crash time. The
        // basename is shared (`format::HELPER_FILE_NAME`) so the crash reporter
        // can recognise the helper as its parent process.
        let tmp = std::env::temp_dir().join(crate::HELPER_FILE_NAME);
        put_cstr(&mut HELPER_PATH, tmp.to_string_lossy().as_bytes());

        if let Ok(exe) = std::env::current_exe() {
            put_cstr(&mut MAIN_EXE, exe.to_string_lossy().as_bytes());
        }

        let task = mach_task_self();

        // S — the handshake port, registered in the bootstrap namespace.
        let mut s: mach_port_t = MACH_PORT_NULL;
        chk(
            mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &mut s),
            "allocate S",
        )?;
        chk(
            mach_port_insert_right(task, s, s, MACH_MSG_TYPE_MAKE_SEND),
            "S insert send",
        )?;
        let cname = CString::new(name).unwrap();
        let mut registered: mach_port_t = MACH_PORT_NULL;
        let mut kr = bootstrap_check_in(bootstrap_port, cname.as_ptr(), &mut registered);
        if kr == BOOTSTRAP_UNKNOWN_SERVICE as kern_return_t {
            let mut ignore = MACH_PORT_NULL;
            bootstrap_create_service(bootstrap_port, cname.as_ptr(), &mut ignore);
            kr = bootstrap_check_in(bootstrap_port, cname.as_ptr(), &mut registered);
        }
        if kr != BOOTSTRAP_SUCCESS as kern_return_t {
            return Err(crate::library::InstallError::Os(format!(
                "bootstrap registration failed ({kr}) — is the app sandboxed?"
            )));
        }
        // `bootstrap_check_in` hands back the receive right; use it as S.
        RECV_PORT = registered;
        let _ = s;

        // E — the task exception port.
        let mut e: mach_port_t = MACH_PORT_NULL;
        chk(
            mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &mut e),
            "allocate E",
        )?;
        chk(
            mach_port_insert_right(task, e, e, MACH_MSG_TYPE_MAKE_SEND),
            "E insert send",
        )?;
        EXC_PORT = e;

        let behavior = (EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES) as exception_behavior_t;
        chk(
            task_set_exception_ports(task, EXC_MASK, e, behavior, 0),
            "task_set_exception_ports",
        )?;

        // Make the watchdog a clean exit rather than a signal-termination.
        libc::signal(libc::SIGALRM, on_sigalrm as *const () as libc::sighandler_t);

        // Resolve every crash-path libc entry point now, via `dlsym`, while
        // dyld's lock is uncontended (see `CrashFns`).
        CRASH_FNS = Some(resolve_crash_fns());

        std::thread::Builder::new()
            .name("dtbke-crash-handler".into())
            .stack_size(256 * 1024)
            .spawn(|| handler_loop())
            .map_err(|err| crate::library::InstallError::Os(err.to_string()))?;
    }
    Ok(())
}

extern "C" fn on_sigalrm(_: libc::c_int) {
    die(134)
}

// ── crash-path libc, resolved up front ─────────────────────────────────
//
// Every libc call `handle()` makes is looked up with `dlsym` in `install()`
// (uncontended) and stashed here. `dlsym` returns the *final bound address*, so
// calling through the stored pointer emits a plain `blr` — it never runs
// `dyld_stub_binder`, which on the crash path can deadlock on dyld's lock (see
// `die`). `mach_msg` is already bound (the handler thread blocks on it before
// any crash); `_exit` is handled by `die` (a raw syscall). `fork` is still not
// fully safe — its `pthread_atfork` prepare handlers take the dyld lock no
// matter how `fork` itself was bound — so the `SIGALRM` watchdog stays the
// backstop for that one.

struct CrashFns {
    open: unsafe extern "C" fn(*const libc::c_char, libc::c_int, ...) -> libc::c_int,
    write: unsafe extern "C" fn(libc::c_int, *const libc::c_void, usize) -> isize,
    fchmod: unsafe extern "C" fn(libc::c_int, libc::mode_t) -> libc::c_int,
    close: unsafe extern "C" fn(libc::c_int) -> libc::c_int,
    pipe: unsafe extern "C" fn(*mut libc::c_int) -> libc::c_int,
    fork: unsafe extern "C" fn() -> libc::pid_t,
    dup2: unsafe extern "C" fn(libc::c_int, libc::c_int) -> libc::c_int,
    execv: unsafe extern "C" fn(*const libc::c_char, *const *const libc::c_char) -> libc::c_int,
    alarm: unsafe extern "C" fn(libc::c_uint) -> libc::c_uint,
    waitpid: unsafe extern "C" fn(libc::pid_t, *mut libc::c_int, libc::c_int) -> libc::pid_t,
}

static mut CRASH_FNS: Option<CrashFns> = None;

unsafe fn resolve_crash_fns() -> CrashFns {
    macro_rules! sym {
        ($name:literal, $fallback:path, $ty:ty) => {{
            let p = libc::dlsym(
                libc::RTLD_DEFAULT,
                concat!($name, "\0").as_ptr().cast::<libc::c_char>(),
            );
            if p.is_null() {
                // `dlsym` can't realistically miss a standard libc symbol, but
                // if it did, address-of forces a non-lazy GOT slot anyway.
                $fallback as $ty
            } else {
                core::mem::transmute::<*mut libc::c_void, $ty>(p)
            }
        }};
    }
    CrashFns {
        open: sym!(
            "open",
            libc::open,
            unsafe extern "C" fn(*const libc::c_char, libc::c_int, ...) -> libc::c_int
        ),
        write: sym!(
            "write",
            libc::write,
            unsafe extern "C" fn(libc::c_int, *const libc::c_void, usize) -> isize
        ),
        fchmod: sym!(
            "fchmod",
            libc::fchmod,
            unsafe extern "C" fn(libc::c_int, libc::mode_t) -> libc::c_int
        ),
        close: sym!(
            "close",
            libc::close,
            unsafe extern "C" fn(libc::c_int) -> libc::c_int
        ),
        pipe: sym!(
            "pipe",
            libc::pipe,
            unsafe extern "C" fn(*mut libc::c_int) -> libc::c_int
        ),
        fork: sym!("fork", libc::fork, unsafe extern "C" fn() -> libc::pid_t),
        dup2: sym!(
            "dup2",
            libc::dup2,
            unsafe extern "C" fn(libc::c_int, libc::c_int) -> libc::c_int
        ),
        execv: sym!(
            "execv",
            libc::execv,
            unsafe extern "C" fn(*const libc::c_char, *const *const libc::c_char) -> libc::c_int
        ),
        alarm: sym!(
            "alarm",
            libc::alarm,
            unsafe extern "C" fn(libc::c_uint) -> libc::c_uint
        ),
        waitpid: sym!(
            "waitpid",
            libc::waitpid,
            unsafe extern "C" fn(libc::pid_t, *mut libc::c_int, libc::c_int) -> libc::pid_t
        ),
    }
}

/// The resolved crash-path libc. Set in `install()` before the handler thread is
/// spawned, so it is always `Some` here.
#[inline(always)]
unsafe fn fns() -> &'static CrashFns {
    CRASH_FNS.as_ref().unwrap_unchecked()
}

/// Terminate the whole process *now*, bypassing libc.
///
/// The crash path runs while another thread — possibly the kernel-suspended
/// faulting thread, or a thread exiting through dyld's TLS-destructor machinery
/// — holds dyld's internal lock. `libc::_exit` is only ever reached on this
/// path, so its lazy-bind stub is unresolved, and resolving it now calls
/// `dyld_stub_binder`, which blocks on that same lock → the process wedges
/// forever (the `SIGALRM` watchdog hit the identical stub). A direct `svc`
/// touches no PLT entry and always works.
#[inline(always)]
pub(crate) fn die(code: i32) -> ! {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        // Darwin/arm64: syscall number in x16, args in x0…, `svc #0x80`.
        // `SYS_exit` (1) terminates every thread in the task.
        core::arch::asm!(
            "mov x16, #1",
            "svc #0x80",
            in("x0") code,
            options(noreturn, nostack),
        );
    }
    #[cfg(not(target_arch = "aarch64"))]
    unsafe {
        libc::_exit(code)
    }
}

fn chk(kr: kern_return_t, what: &str) -> Result<(), crate::library::InstallError> {
    if kr == KERN_SUCCESS {
        Ok(())
    } else {
        Err(crate::library::InstallError::Os(format!(
            "{what}: kern_return {kr}"
        )))
    }
}

unsafe fn put_cstr(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
}

// ── the handler thread ─────────────────────────────────────────────────

#[repr(C, align(8))]
struct MsgBuf([u8; 1024]);

fn handler_loop() -> ! {
    loop {
        let mut buf = MsgBuf([0; 1024]);
        let kr = unsafe {
            mach_msg(
                buf.0.as_mut_ptr() as *mut mach_msg_header_t,
                MACH_RCV_MSG,
                0,
                buf.0.len() as u32,
                EXC_PORT,
                MACH_MSG_TIMEOUT_NONE,
                MACH_PORT_NULL,
            )
        };
        if kr != MACH_MSG_SUCCESS {
            continue;
        }
        if rd_i32(&buf.0, 20) != MSG_ID_EXCEPTION_RAISE {
            continue;
        }
        if HANDLED.swap(true, Ordering::SeqCst) {
            die(134);
        }
        unsafe {
            (fns().alarm)(WATCHDOG_SECS);
            handle(&buf.0);
            die(134);
        }
    }
}

fn rd_i32(b: &[u8], o: usize) -> i32 {
    i32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_u32(b: &[u8], o: usize) -> u32 {
    u32::from_ne_bytes(b[o..o + 4].try_into().unwrap())
}
fn rd_i64(b: &[u8], o: usize) -> i64 {
    i64::from_ne_bytes(b[o..o + 8].try_into().unwrap())
}

/// `mach_exception_raise` request layout (see the byte-offset table in the
/// previous revision): thread port at 28, task port at 40, exception at 60,
/// code[2] at 68.
unsafe fn handle(msg: &[u8]) {
    let thread_port: mach_port_t = rd_u32(msg, 28);
    let task_port: mach_port_t = rd_u32(msg, 40);
    let exception = rd_i32(msg, 60) as u32;
    let code0 = rd_i64(msg, 68) as u64;
    let code1 = rd_i64(msg, 76) as u64;

    let f = fns();

    dtb_ke_log::fault!("------------[ cut here ]------------");
    crate::oops_art();
    if crate::panic::panicking() {
        dtb_ke_log::fault!("Rust panic! Trapping to an out-of-process minidump");
    } else {
        dtb_ke_log::fault!(
            "Mach exception {exception:#x}, code {code0:#x}/{code1:#x}, capturing a minidump"
        );
    }

    extract_helper();

    // The helper is spawned with a bare argv (argv[0] only). Its parameters —
    // dump dir, slug, bootstrap name, panic message (empty for a hardware fault)
    // — go down a pipe on stdin as NUL-separated fields. The Mach handoff carries
    // the port rights + the exception triple.
    let mut pfd = [0i32; 2];
    if (f.pipe)(pfd.as_mut_ptr()) != 0 {
        dtb_ke_log::fault!("Could not open the helper pipe, no minidump");
        dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
        return;
    }

    let child = (f.fork)();
    if child == 0 {
        // ── child ──
        (f.dup2)(pfd[0], 0);
        (f.close)(pfd[0]);
        (f.close)(pfd[1]);
        let argv: [*const libc::c_char; 2] = [HELPER_PATH.as_ptr() as *const _, std::ptr::null()];
        (f.execv)(HELPER_PATH.as_ptr() as *const _, argv.as_ptr());
        die(127);
    }
    if child < 0 {
        dtb_ke_log::fault!("Could not fork the crash helper, no minidump");
        (f.close)(pfd[0]);
        (f.close)(pfd[1]);
        dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
        return;
    }

    // ── parent: feed the pipe, then close it so the helper sees EOF ──
    // Fields: dump_dir \0 slug \0 bootstrap_name \0 main_exe \0 panic_msg.
    (f.close)(pfd[0]);
    write_cstr(pfd[1], DUMP_DIR.as_ptr());
    write_cstr(pfd[1], APP_SLUG.as_ptr());
    write_cstr(pfd[1], BOOTSTRAP_NAME.as_ptr());
    write_cstr(pfd[1], MAIN_EXE.as_ptr());
    // The panic message static is a NUL-filled buffer when not panicking, so this
    // is just an empty field for a hardware fault.
    write_cstr(pfd[1], crate::panic::message_ptr());
    (f.close)(pfd[1]);

    // ── parent: the mach handoff ──
    // 1. receive the helper's HELLO carrying its reply port R. Time-bounded: if
    //    the helper failed to `execv` or never reached `bootstrap_look_up`, this
    //    must not wedge the handler thread until the `SIGALRM` watchdog.
    let mut rbuf = MsgBuf([0; 1024]);
    if mach_msg(
        rbuf.0.as_mut_ptr() as *mut mach_msg_header_t,
        MACH_RCV_MSG | MACH_RCV_TIMEOUT,
        0,
        rbuf.0.len() as u32,
        RECV_PORT,
        RECV_TIMEOUT_MS,
        MACH_PORT_NULL,
    ) != MACH_MSG_SUCCESS
    {
        dtb_ke_log::fault!("Crash helper did not connect within {RECV_TIMEOUT_MS}ms, no minidump");
        // reap the child so it can't linger as a zombie, then bail.
        let mut st = 0i32;
        (f.waitpid)(child, &mut st, 0);
        dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
        return;
    }
    if rd_i32(&rbuf.0, 20) != MSG_ID_HELPER_HELLO {
        return;
    }
    // complex msg: header(24) + body(4) + port descriptor(12); .name at +28.
    let reply_port: mach_port_t = rd_u32(&rbuf.0, 28);

    // 2. forward the crashed task + faulting thread + this handler thread, plus
    //    the exception triple, to R. The helper feeds these straight into a
    //    `crash_context::CrashContext` for `minidump-writer`.
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
    let mut fwd: Forward = zeroed();
    fwd.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
    fwd.head.msgh_size = size_of::<Forward>() as u32;
    fwd.head.msgh_remote_port = reply_port;
    fwd.head.msgh_local_port = MACH_PORT_NULL;
    fwd.head.msgh_id = MSG_ID_FORWARD;
    fwd.body.msgh_descriptor_count = 3;
    fwd.task = mach_msg_port_descriptor_t::new(task_port, MACH_MSG_TYPE_COPY_SEND);
    fwd.thread = mach_msg_port_descriptor_t::new(thread_port, MACH_MSG_TYPE_COPY_SEND);
    fwd.handler_thread = mach_msg_port_descriptor_t::new(
        mach2::mach_init::mach_thread_self(),
        MACH_MSG_TYPE_COPY_SEND,
    );
    fwd.exception = exception;
    fwd.code = code0;
    fwd.subcode = code1;
    let _ = mach_msg(
        &mut fwd.head,
        MACH_SEND_MSG,
        size_of::<Forward>() as u32,
        0,
        MACH_PORT_NULL,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    );

    // 3. wait (bounded) for the helper's HELPER_DONE — it sends this the moment
    //    it has finished `mach_vm_read`ing our memory. Then return so the caller
    //    `_exit(134)`s *at once*: we must not pin the crashed process (and its
    //    frozen windows) on screen for the whole lifetime of the crash dialog.
    //    The helper is reparented to launchd and finishes on its own — write the
    //    dump file, launch the reporter, act on its verdict.
    //    `RECV_TIMEOUT_MS` + the `SIGALRM` watchdog bound this if the helper died.
    let mut dbuf = MsgBuf([0; 1024]);
    let _ = mach_msg(
        dbuf.0.as_mut_ptr() as *mut mach_msg_header_t,
        MACH_RCV_MSG | MACH_RCV_TIMEOUT,
        0,
        dbuf.0.len() as u32,
        RECV_PORT,
        RECV_TIMEOUT_MS,
        MACH_PORT_NULL,
    );
    dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
    // then `_exit(134)` (caller does it).
}

/// Extract `HELPER_BYTES` to `HELPER_PATH` — `open`/`write`/`fchmod`/`close`, no
/// allocation.
unsafe fn extract_helper() {
    let f = fns();
    let fd = (f.open)(
        HELPER_PATH.as_ptr() as *const _,
        libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
        0o700 as libc::c_int,
    );
    if fd < 0 {
        return;
    }
    let mut off = 0usize;
    while off < HELPER_BYTES.len() {
        let n = (f.write)(
            fd,
            HELPER_BYTES.as_ptr().add(off) as *const _,
            HELPER_BYTES.len() - off,
        );
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
    (f.fchmod)(fd, 0o700);
    (f.close)(fd);
}

/// Write a NUL-terminated C string plus its terminating NUL (the field
/// separator) to `fd`. Allocation-free.
unsafe fn write_cstr(fd: i32, p: *const u8) {
    let write = fns().write;
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    let total = len + 1; // include the NUL
    let mut off = 0usize;
    while off < total {
        let n = write(fd, p.add(off) as *const _, total - off);
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}
