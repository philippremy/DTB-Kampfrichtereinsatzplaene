//! iOS crash capture — entirely in-process (a sandboxed app cannot spawn a helper).
//!
//! `install()` (clean context): pick a launch id, write the `.session` sidecar (OS facts + loaded
//! images; kept fresh by a background thread), pre-touch every function the crash path uses
//! ([`apple::capture::warm_up`]), point the task exception ports at a port `E` and spawn a thread
//! blocked on it. A Rust panic traps with `brk`, so it reaches `E` like a hardware fault. Software-only
//! signals with no hardware trap of their own (`FATAL_SIGNALS` — `SIGABRT`, `SIGSYS`, `SIGXCPU`,
//! `SIGXFSZ`, `SIGEMT`, `SIGPIPE`) are instead caught via `sigaction` and fed straight into
//! [`handle_exception`] as a synthesized `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` exception (subcode = the real
//! signal number) — the same technique, and the same reasoning for it, as the macOS side of this crate
//! (see its module doc comment): no Mach exception genuinely reaches a task port for these, and the
//! kernel's *own* `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` delivery is `ptrace`-only, unavailable without a
//! permanently-running second process. This *replaces* an earlier approach here that converted `SIGABRT`
//! into a fake `brk` trap — correctness-equivalent for capture, but it mislabeled the snapshot's own
//! exception record (and the system's crash report, once re-raised) with the wrong signal.
//!
//! On a fault the handler thread — syscalls, Mach traps, fixed buffers and raw `write`, nothing else —
//! writes `<slug>-<launch>.crash` via [`apple::capture::capture`], then replies `KERN_FAILURE` to the
//! exception message ("not handled here") so the kernel continues down the normal path: the system's
//! own crash report (TestFlight / Xcode Organizer) is still produced and the process ends as it would
//! have without us. The synthetic signal path preserves the same intent without a fake Mach message:
//! after capturing, it resets the signal to `SIG_DFL` and re-raises it, so the real default disposition
//! — and whatever the system's own crash-observability hooks into for that — still runs. **Nothing is
//! converted at crash time**: at the next launch the app turns the snapshot into a minidump
//! (`snapshot::to_minidump`) and offers to send it.
//!
//! Not caught (no Mach exception reaches a task port, and no signal handler intercepts them): jetsam /
//! out-of-memory kills, watchdog terminations, force-quit, `SIGKILL`. With a debugger attached, lldb
//! takes the exceptions first.

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::ffi::CString;
use std::mem::zeroed;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use mach2::exception_types::{
    EXC_MASK_ARITHMETIC, EXC_MASK_BAD_ACCESS, EXC_MASK_BAD_INSTRUCTION, EXC_MASK_BREAKPOINT,
    EXC_SOFT_SIGNAL, EXC_SOFTWARE, EXCEPTION_DEFAULT, MACH_EXCEPTION_CODES, exception_behavior_t,
    exception_mask_t,
};
use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
use mach2::message::{
    MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_MAKE_SEND, MACH_MSG_TYPE_MOVE_SEND_ONCE,
    MACH_MSGH_BITS, MACH_RCV_MSG, MACH_SEND_MSG, mach_msg, mach_msg_header_t,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
use mach2::thread_status::thread_state_flavor_t;
use mach2::traps::mach_task_self;

use crate::apple::{capture, die, session};
use crate::snapshot;

/// MIG id of the `mach_exception_raise` request.
const MSG_ID_EXCEPTION_RAISE: i32 = 2405;
const EXC_MASK: exception_mask_t =
    EXC_MASK_BAD_ACCESS | EXC_MASK_BAD_INSTRUCTION | EXC_MASK_ARITHMETIC | EXC_MASK_BREAKPOINT;
/// Software-only signals with no Mach exception of their own — see the module doc comment. Every one
/// of the classic BSD "core-dumping" signals not already covered by a real hardware fault (`SIGQUIT`
/// deliberately excluded — user/operator-initiated, not a crash), plus `SIGPIPE` (default disposition
/// is also fatal, and realistically triggerable — a socket/pipe write with no reader). Mirrors the
/// macOS side of this crate exactly.
const FATAL_SIGNALS: [libc::c_int; 6] = [
    libc::SIGABRT,
    libc::SIGSYS,
    libc::SIGXCPU,
    libc::SIGXFSZ,
    libc::SIGEMT,
    libc::SIGPIPE,
];
/// Absolute backstop: if capturing wedges, `SIGALRM` ends the process cleanly.
const WATCHDOG_SECS: libc::c_uint = 12;
/// How often the sidecar is refreshed while images are still loading.
const SESSION_REFRESH_SECS: u64 = 2;

static mut EXC_PORT: mach_port_t = MACH_PORT_NULL;
static mut CRASH_PATH: [u8; 1024] = [0; 1024];
static mut LAUNCH_ID: [u8; 16] = [0; 16];
static HANDLED: AtomicBool = AtomicBool::new(false);
static IMAGE_EPOCH: AtomicU32 = AtomicU32::new(0);

unsafe extern "C" {
    fn task_set_exception_ports(
        task: mach_port_t,
        exception_mask: exception_mask_t,
        new_port: mach_port_t,
        behavior: exception_behavior_t,
        new_flavor: thread_state_flavor_t,
    ) -> kern_return_t;
    fn _dyld_register_func_for_add_image(func: extern "C" fn(*const u8, isize));
}

pub(crate) fn install(dump_dir: &Path, app_slug: &str) -> Result<(), crate::library::InstallError> {
    let os = |what: &str, kr: kern_return_t| {
        crate::library::InstallError::Os(format!("{what}: kern_return {kr}"))
    };

    unsafe {
        libc::arc4random_buf(LAUNCH_ID.as_mut_ptr().cast(), 16);
        let hex: String = LAUNCH_ID.iter().map(|b| format!("{b:02x}")).collect();
        let stem = dump_dir.join(format!("{app_slug}-{hex}"));
        let crash = CString::new(stem.with_extension("crash").to_string_lossy().as_bytes())
            .map_err(|e| crate::library::InstallError::Os(e.to_string()))?;
        let bytes = crash.as_bytes_with_nul();
        if bytes.len() > CRASH_PATH.len() {
            return Err(crate::library::InstallError::Os("dump path too long".into()));
        }
        CRASH_PATH[..bytes.len()].copy_from_slice(bytes);

        // The sidecar first, so it exists even if the very first second crashes.
        let session_path = stem.with_extension("session");
        let launch_id = LAUNCH_ID;
        let version = env!("CARGO_PKG_VERSION");
        if let Err(err) = snapshot::session::write(&session_path, &session::current(launch_id, version)) {
            eprintln!("dtb-ke-crash: could not write the session sidecar: {err}");
        }
        _dyld_register_func_for_add_image(on_image_added);
        std::thread::Builder::new()
            .name("dtbke-crash-session".into())
            .spawn(move || session_refresh_loop(session_path, launch_id, version))
            .map_err(|e| crate::library::InstallError::Os(e.to_string()))?;

        capture::warm_up();

        let task = mach_task_self();
        let mut e: mach_port_t = MACH_PORT_NULL;
        let kr = mach_port_allocate(task, MACH_PORT_RIGHT_RECEIVE, &mut e);
        if kr != KERN_SUCCESS {
            return Err(os("allocate E", kr));
        }
        let kr = mach_port_insert_right(task, e, e, MACH_MSG_TYPE_MAKE_SEND);
        if kr != KERN_SUCCESS {
            return Err(os("E insert send", kr));
        }
        EXC_PORT = e;
        let behavior = (EXCEPTION_DEFAULT | MACH_EXCEPTION_CODES) as exception_behavior_t;
        let kr = task_set_exception_ports(task, EXC_MASK, e, behavior, 0);
        if kr != KERN_SUCCESS {
            return Err(os("task_set_exception_ports", kr));
        }

        libc::signal(libc::SIGALRM, on_sigalrm as *const () as libc::sighandler_t);
        for sig in FATAL_SIGNALS {
            libc::signal(sig, on_fatal_signal as *const () as libc::sighandler_t);
        }

        std::thread::Builder::new()
            .name("dtbke-crash-handler".into())
            .stack_size(256 * 1024)
            .spawn(|| handler_loop())
            .map_err(|err| crate::library::InstallError::Os(err.to_string()))?;
    }
    Ok(())
}

/// The path of this launch's `.session` sidecar's sibling `.crash` file (for the app to skip its own).
pub fn own_session_path(dump_dir: &Path, app_slug: &str) -> Option<std::path::PathBuf> {
    let hex: String = unsafe { LAUNCH_ID.iter().map(|b| format!("{b:02x}")).collect() };
    Some(dump_dir.join(format!("{app_slug}-{hex}")).with_extension("session"))
}

extern "C" fn on_image_added(_header: *const u8, _slide: isize) {
    IMAGE_EPOCH.fetch_add(1, Ordering::Relaxed);
}

fn session_refresh_loop(path: std::path::PathBuf, launch_id: [u8; 16], version: &'static str) {
    let mut written = IMAGE_EPOCH.load(Ordering::Relaxed);
    loop {
        std::thread::sleep(std::time::Duration::from_secs(SESSION_REFRESH_SECS));
        let now = IMAGE_EPOCH.load(Ordering::Relaxed);
        if now != written {
            written = now;
            let _ = snapshot::session::write(&path, &session::current(launch_id, version));
        }
    }
}

extern "C" fn on_sigalrm(_: libc::c_int) {
    die(134)
}

/// `FATAL_SIGNALS` handler — see the module doc comment for why this exists at all. Builds the same
/// `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` exception shape XNU itself uses for signal delivery (subcode = the
/// real signal number) and feeds it straight into `handle_exception`, exactly as if it had arrived as a
/// genuine Mach exception message. Mirrors the macOS side of this crate, except for what happens
/// afterward: macOS just `die()`s; here, to preserve `handler_loop`'s own `reply_not_handled` intent
/// (let the system's own crash report still happen), reset the signal to `SIG_DFL` and re-raise it —
/// simpler and more accurate than round-tripping through a hand-crafted Mach message just to reach the
/// same `reply_not_handled` code path, and (unlike the `brk`-trap approach this replaced) the system's
/// own report now names the real signal too, not a substituted `SIGTRAP`.
extern "C" fn on_fatal_signal(signo: libc::c_int) {
    if HANDLED.swap(true, Ordering::SeqCst) {
        die(134);
    }
    unsafe {
        libc::alarm(WATCHDOG_SECS);
        let me = mach2::mach_init::mach_thread_self();
        handle_exception(me, me, EXC_SOFTWARE, u64::from(EXC_SOFT_SIGNAL), signo as u64);
        libc::alarm(0);
        libc::signal(signo, libc::SIG_DFL);
        libc::raise(signo);
    }
    // Backstop: the re-raise above should never return.
    die(134);
}

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
        if kr != MACH_MSG_SUCCESS || rd_i32(&buf.0, 20) != MSG_ID_EXCEPTION_RAISE {
            continue;
        }
        if HANDLED.swap(true, Ordering::SeqCst) {
            // A second fault while handling the first (or in another thread): give up quietly.
            die(134);
        }
        unsafe {
            libc::alarm(WATCHDOG_SECS);
            handle(&buf.0);
            libc::alarm(0);
            reply_not_handled(&buf.0);
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

/// `mach_exception_raise` request layout (as in the macOS handler): thread port at 28, task port at
/// 40, exception at 60, code[2] at 68. Parses a genuine kernel-delivered message and hands off to
/// `handle_exception` — the handler thread doing the receiving *is* the "handler thread" in the usual
/// sense here, unlike `on_fatal_signal`'s synthetic call.
unsafe fn handle(msg: &[u8]) {
    let thread_port: mach_port_t = rd_u32(msg, 28);
    let exception = rd_i32(msg, 60) as u32;
    let code0 = rd_i64(msg, 68) as u64;
    let code1 = rd_i64(msg, 76) as u64;
    handle_exception(
        thread_port,
        mach2::mach_init::mach_thread_self(),
        exception,
        code0,
        code1,
    );
}

/// The shared core: given an exception already reduced to its crashing/handler-thread ports +
/// kind/code/subcode — whether parsed from a real Mach exception message (`handle` above) or
/// synthesized ourselves (`on_fatal_signal`) — write the crash snapshot. Both call sites are
/// responsible for whatever happens after (`handler_loop`'s `reply_not_handled`, or
/// `on_fatal_signal`'s re-raise).
unsafe fn handle_exception(
    crashing_thread: mach_port_t,
    handler_thread: mach_port_t,
    exception: u32,
    code0: u64,
    code1: u64,
) {
    dtb_ke_log::fault!("------------[ cut here ]------------");
    crate::oops_art();
    if crate::panic::panicking() {
        dtb_ke_log::fault!("Rust panic! Writing a crash snapshot");
    } else if exception == EXC_SOFTWARE && code0 == u64::from(EXC_SOFT_SIGNAL) {
        // Our own synthesized exception (`on_fatal_signal`) — code1 is the real signal number.
        match code1 as i32 {
            libc::SIGABRT => dtb_ke_log::fault!("abort() (SIGABRT), writing a crash snapshot"),
            libc::SIGSYS => dtb_ke_log::fault!("SIGSYS, writing a crash snapshot"),
            libc::SIGXCPU => dtb_ke_log::fault!("SIGXCPU (CPU time limit), writing a crash snapshot"),
            libc::SIGXFSZ => dtb_ke_log::fault!("SIGXFSZ (file size limit), writing a crash snapshot"),
            libc::SIGEMT => dtb_ke_log::fault!("SIGEMT, writing a crash snapshot"),
            libc::SIGPIPE => dtb_ke_log::fault!("SIGPIPE, writing a crash snapshot"),
            _ => dtb_ke_log::fault!("signal {code1}, writing a crash snapshot"),
        }
    } else {
        dtb_ke_log::fault!("Mach exception {exception:#x}, code {code0:#x}/{code1:#x}, writing a crash snapshot");
    }

    let fd = libc::open(
        CRASH_PATH.as_ptr() as *const _,
        libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
        0o600 as libc::c_int,
    );
    if fd < 0 {
        dtb_ke_log::fault!("Could not open the crash file, no snapshot");
        dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
        return;
    }
    capture::capture(&capture::Params {
        fd,
        launch_id: LAUNCH_ID,
        exception: Some((exception, code0, code1)),
        crashing_thread,
        handler_thread,
        panic_message: crate::panic::message_ptr(),
        resume: false,
    });
    libc::close(fd);
    dtb_ke_log::fault!(" ---[ end trace {exception:#x} ]---");
}

/// Reply `KERN_FAILURE` ("this handler did not handle it"): the kernel moves on to the next handler
/// (the system's crash reporter) and the process ends as it would have without us. `msgh_remote_port`
/// of the request is the send-once reply right.
unsafe fn reply_not_handled(request: &[u8]) {
    #[repr(C)]
    struct Reply {
        head: mach_msg_header_t,
        ndr: [u8; 8],
        ret_code: kern_return_t,
    }
    let mut reply: Reply = zeroed();
    reply.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_MOVE_SEND_ONCE, 0);
    reply.head.msgh_size = core::mem::size_of::<Reply>() as u32;
    reply.head.msgh_remote_port = rd_u32(request, 8);
    reply.head.msgh_local_port = MACH_PORT_NULL;
    reply.head.msgh_id = MSG_ID_EXCEPTION_RAISE + 100;
    // NDR_record: little-endian integers, ASCII chars, IEEE floats.
    reply.ndr = [0, 0, 0, 0, 1, 0, 0, 0];
    reply.ret_code = 5; // KERN_FAILURE
    let _ = mach_msg(
        &mut reply.head,
        MACH_SEND_MSG,
        core::mem::size_of::<Reply>() as u32,
        0,
        MACH_PORT_NULL,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    );
}
