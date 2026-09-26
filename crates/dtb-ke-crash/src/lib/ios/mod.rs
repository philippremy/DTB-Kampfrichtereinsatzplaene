//! iOS crash capture — entirely in-process (a sandboxed app cannot spawn a helper).
//!
//! `install()` (clean context): pick a launch id, write the `.session` sidecar (OS facts + loaded
//! images; kept fresh by a background thread), pre-touch every function the crash path uses
//! ([`apple::capture::warm_up`]), point the task exception ports at a port `E` and spawn a thread
//! blocked on it. A Rust panic traps with `brk` (the same bridge as macOS) and `SIGABRT` is turned
//! into the same trap, so aborts and panics reach `E` like a hardware fault.
//!
//! On a fault the handler thread — syscalls, Mach traps, fixed buffers and raw `write`, nothing else —
//! writes `<slug>-<launch>.crash` via [`apple::capture::capture`], then replies `KERN_FAILURE` to the
//! exception message ("not handled here") so the kernel continues down the normal path: the system's
//! own crash report (TestFlight / Xcode Organizer) is still produced and the process ends as it would
//! have without us. **Nothing is converted at crash time**: at the next launch the app turns the
//! snapshot into a minidump (`snapshot::to_minidump`) and offers to send it.
//!
//! Not caught (no Mach exception reaches a task port): jetsam / out-of-memory kills, watchdog
//! terminations, force-quit, `SIGKILL`. With a debugger attached, lldb takes the exceptions first.

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::ffi::CString;
use std::mem::zeroed;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use mach2::exception_types::{
    EXC_MASK_ARITHMETIC, EXC_MASK_BAD_ACCESS, EXC_MASK_BAD_INSTRUCTION, EXC_MASK_BREAKPOINT,
    EXCEPTION_DEFAULT, MACH_EXCEPTION_CODES, exception_behavior_t, exception_mask_t,
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
/// Absolute backstop: if capturing wedges, `SIGALRM` ends the process cleanly.
const WATCHDOG_SECS: libc::c_uint = 12;
/// How often the sidecar is refreshed while images are still loading.
const SESSION_REFRESH_SECS: u64 = 2;

static mut EXC_PORT: mach_port_t = MACH_PORT_NULL;
static mut CRASH_PATH: [u8; 1024] = [0; 1024];
static mut LAUNCH_ID: [u8; 16] = [0; 16];
static HANDLED: AtomicBool = AtomicBool::new(false);
/// Set by the `SIGABRT` handler so the Mach handler can log that the trap came from an abort.
static ABORTING: AtomicBool = AtomicBool::new(false);
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
        libc::signal(libc::SIGABRT, on_sigabrt as *const () as libc::sighandler_t);

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

/// `abort()` (a C library, `handle_alloc_error`, an unhandled Objective-C exception, …) raises
/// `SIGABRT`, which no Mach exception port sees. Turn it into the same `brk` trap the panic hook uses
/// so it takes the normal path. The system report then says `SIGTRAP`; the snapshot's stack still
/// starts at the abort.
extern "C" fn on_sigabrt(_: libc::c_int) {
    ABORTING.store(true, Ordering::SeqCst);
    unsafe { core::arch::asm!("brk #0xdb", options(nostack)) };
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
/// 40, exception at 60, code[2] at 68.
unsafe fn handle(msg: &[u8]) {
    let thread_port: mach_port_t = rd_u32(msg, 28);
    let exception = rd_i32(msg, 60) as u32;
    let code0 = rd_i64(msg, 68) as u64;
    let code1 = rd_i64(msg, 76) as u64;

    dtb_ke_log::fault!("------------[ cut here ]------------");
    crate::oops_art();
    if crate::panic::panicking() {
        dtb_ke_log::fault!("Rust panic! Writing a crash snapshot");
    } else if ABORTING.load(Ordering::SeqCst) {
        dtb_ke_log::fault!("abort() (SIGABRT), writing a crash snapshot");
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
        crashing_thread: thread_port,
        handler_thread: mach2::mach_init::mach_thread_self(),
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
