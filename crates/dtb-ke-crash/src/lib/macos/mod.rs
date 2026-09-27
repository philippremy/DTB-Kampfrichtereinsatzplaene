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
    EXC_SOFT_SIGNAL, EXC_SOFTWARE, EXCEPTION_DEFAULT, MACH_EXCEPTION_CODES, exception_behavior_t,
    exception_mask_t,
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

// `FATAL_SIGNALS` are pure software signals — no hardware trap, so no Mach exception of their own
// ever reaches a task-level port here, verified two ways: `EXC_MASK_CRASH` registers successfully
// but is never fed for a plain `abort()` (it's host-port-scoped in practice, reserved for the
// system's own crash reporter); `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` — the constants Apple's own kernel
// *does* use for signal delivery — checked against XNU's published source (`bsd/kern/kern_sig.c`'s
// `do_bsdexception` call) turned out to be gated behind `P_LTRACED`: the kernel only ever raises it
// for a process being actively `ptrace`d by a separate debugger process. Neither is available to us
// without a permanently-running second process, which we don't want. `on_fatal_signal` below is the
// real fix: a `sigaction` for all of them, constructing the *same* `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL`
// exception shape ourselves and feeding it straight into `handle_exception` — an ordinary
// same-process Mach send to a port we already own (no privileged mechanism, no ptrace relationship,
// nothing the kernel wouldn't let any process do to itself) — so the resulting dump correctly names
// the real signal instead of a fabricated hardware trap.
const EXC_MASK: u32 =
    EXC_MASK_BAD_ACCESS | EXC_MASK_BAD_INSTRUCTION | EXC_MASK_ARITHMETIC | EXC_MASK_BREAKPOINT;

/// Software-only signals with no Mach exception of their own — see `EXC_MASK`'s doc comment. Every
/// one of the classic BSD "core-dumping" signals not already covered by a real hardware fault
/// (`SIGQUIT` deliberately excluded — user/operator-initiated, not a crash), plus `SIGPIPE` (default
/// disposition is also fatal, and realistically triggerable — a socket/pipe write with no reader).
const FATAL_SIGNALS: [libc::c_int; 6] = [
    libc::SIGABRT,
    libc::SIGSYS,
    libc::SIGXCPU,
    libc::SIGXFSZ,
    libc::SIGEMT,
    libc::SIGPIPE,
];

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
        // No Mach exception of their own — see `EXC_MASK`'s doc comment and `FATAL_SIGNALS`.
        for sig in FATAL_SIGNALS {
            libc::signal(sig, on_fatal_signal as *const () as libc::sighandler_t);
        }

        // Resolve every crash-path libc entry point now, via `dlsym`, while
        // dyld's lock is uncontended (see `CrashFns`).
        CRASH_FNS = Some(resolve_crash_fns());

        // Diagnostic only: capture an uncaught `NSException`'s name/reason/call stack before it
        // unwinds any further — this changes nothing about the actual Mach-exception capture path
        // below, it just gives the resulting minidump's session log (and, if one gets written, its
        // `nsexception` stream) the human-readable reason a Mach exception alone never carries.
        install_exception_preprocessor();

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

/// `SIGABRT`/`SIGSYS` handler — see `EXC_MASK`'s doc comment for why this exists at all. Builds the
/// same `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` exception shape XNU itself uses for signal delivery (subcode
/// = the real signal number) and feeds it straight into `handle_exception`, exactly as if it had
/// arrived as a genuine Mach exception message — the rest of the capture pipeline neither knows nor
/// cares that this one was synthesized rather than received. Never returns (`handle_exception` is
/// followed by `die()`, mirroring `handler_loop`): a signal handler that returns normally would let
/// the signal's real default disposition (terminate, no dump) race with our own capture.
extern "C" fn on_fatal_signal(signo: libc::c_int) {
    if HANDLED.swap(true, Ordering::SeqCst) {
        die(134);
    }
    unsafe {
        (fns().alarm)(WATCHDOG_SECS);
        // No separate handler thread exists here — the signal runs synchronously on the thread that
        // raised it, which is also the thread we want dumped. `MACH_PORT_NULL` tells
        // `minidump-writer` not to exclude any thread (it otherwise unconditionally skips whichever
        // thread `handler_thread` names — correct when that's a dedicated Mach-exception-plumbing
        // thread with nothing interesting on its stack, wrong here, where it would silently drop the
        // one thread we actually want captured).
        handle_exception(
            mach_task_self(),
            mach2::mach_init::mach_thread_self(),
            MACH_PORT_NULL,
            EXC_SOFTWARE,
            u64::from(EXC_SOFT_SIGNAL),
            signo as u64,
        );
        die(134);
    }
}

// ── uncaught NSException logging ───────────────────────────────────────
//
// `objc_setExceptionPreprocessor` — private, but long-stable, libobjc API: `objc_exception_throw`
// calls it synchronously, at the *throw* site, before the unwind search phase even begins, handing
// over the raw exception object. Verified empirically (a standalone Objective-C program) to fire for
// every `NSException` raised anywhere in the process, on any thread, regardless of what — if
// anything — later catches it. This is the same technique Crashpad (Chromium's crash client) uses
// for exactly this purpose (`client/ios_handler/exception_processor.mm`).
//
// **`-[NSException callStackSymbols]` is useless here — verified empirically, it does not just read
// differently, it is unpopulated.** Apple's own default preprocessor is the thing that normally
// walks the stack and fills in `callStackReturnAddresses` (which `callStackSymbols` is derived from);
// installing a custom preprocessor replaces that default entirely, so nothing fills it in any more —
// confirmed with a standalone Objective-C program: `count` was `0` both inside our preprocessor *and*
// later inside `NSSetUncaughtExceptionHandler`, and even Apple's own "First throw call stack" banner
// printed `(null)`. This is exactly why Crashpad's own preprocessor captures its own trace via
// libunwind rather than relying on the exception object — we do the same (see below).
//
// Deliberately *not* used: `NSSetUncaughtExceptionHandler` (only fires for an exception that
// unwinds all the way past every `@catch` with nothing left to catch it — AppKit's own guarded
// run-loop dispatch swallows plenty that never reach it) and swizzling the private
// `-[NSApplication _crashOnException:]` (AppKit's call for exactly that guarded-and-decided-fatal
// case, but not even a stable *selector* — Apple moved it from an instance method to a class method
// around macOS 26, confirmed against other crash-reporting SDKs hitting the same break —
// getsentry/sentry-cocoa#7136). Both are private-API surface with no more coverage than this one
// hook already provides on its own.
//
// This does not stop or alter unwinding — the exception propagates exactly as it would have. So it
// doesn't change *whether* a minidump gets written (still entirely up to whatever the exception
// eventually causes to reach our Mach exception port below); it only guarantees that whenever one
// is, its `nsexception` stream carries the real name/reason/stack, not just a bare Mach exception
// code.

use objc2::{ffi::{objc_setExceptionPreprocessor}, runtime::AnyObject};
use objc2_foundation::NSException;

/// Taken from obj2::ffi::exception::objc_exception_preprocessor, which is a private module
#[allow(non_camel_case_types)]
type objc_exception_preprocessor = unsafe extern "C" fn(exception: *mut AnyObject) -> *mut AnyObject;

/// Whatever preprocessor (if any) was already installed — chained after our own capture runs, so we
/// compose with anything else that installs one rather than silently discarding it.
static mut PREVIOUS_PREPROCESSOR: Option<objc_exception_preprocessor> = None;

fn install_exception_preprocessor() {
    unsafe {
        PREVIOUS_PREPROCESSOR = Some(objc_setExceptionPreprocessor(intercept_nsexception));
    }
}

extern "C" fn intercept_nsexception(exception: *mut AnyObject) -> *mut AnyObject {
    // SAFETY: `objc_exception_throw` calls this synchronously with the object about to be thrown;
    // we only borrow it (never retained) for the duration of this call.
    if let Some(obj) = unsafe { exception.as_ref() } {
        // `@throw` can legally raise any object, not only an `NSException` — check before treating
        // it as one.
        if let Some(exc) = obj.downcast_ref::<NSException>() {
            capture_nsexception(exc);
        }
    }
    // We never substitute or swallow — always hand back exactly what we were given, either straight
    // through or via whatever preprocessor was chained before ours.
    unsafe {
        if let Some(previous) = PREVIOUS_PREPROCESSOR {
            return previous(exception);
        }
    }
    exception
}

// libunwind's local-unwind API — not `backtrace`/`backtrace_symbols` (a frame-pointer walk): always
// present via `libSystem` on macOS/iOS (`usr/lib/system/libunwind.tbd`; no extra link step, it's a
// hard dependency of `libc++abi`'s own exception unwinding, i.e. of ordinary `@throw`/`raise` itself)
// and it's what Crashpad's own exception preprocessor uses (see the module doc comment). Verified
// empirically before writing this (a standalone C program): struct sizes/alignment are identical on
// arm64 and x86_64 (`unw_context_t` 1336 bytes, `unw_cursor_t` 1632, both 8-aligned — from
// `libunwind.h` on this SDK), so one representation covers both slices of the universal binary; a
// tail-call-eliminated frame is missing from libunwind's walk exactly as it would be from a
// frame-pointer one (also verified — a `-O2` build lost the same intermediate frames either way),
// since the frame genuinely no longer exists on the stack, which is not something either technique
// can recover — libunwind's real advantage here is everywhere else: it walks the compiler-emitted
// unwind info (`__unwind_info`/DWARF CFI) instead of assuming the frame-pointer convention was
// actually followed at every single call site.

#[repr(C, align(8))]
struct UnwContext([u8; 1336]);
#[repr(C, align(8))]
struct UnwCursor([u8; 1632]);

const UNW_REG_IP: i32 = -1;

unsafe extern "C" {
    fn unw_getcontext(ctx: *mut UnwContext) -> i32;
    fn unw_init_local(cursor: *mut UnwCursor, ctx: *mut UnwContext) -> i32;
    fn unw_step(cursor: *mut UnwCursor) -> i32;
    fn unw_get_reg(cursor: *mut UnwCursor, reg: i32, value: *mut u64) -> i32;
    fn unw_get_proc_name(
        cursor: *mut UnwCursor,
        buf: *mut libc::c_char,
        len: usize,
        offset: *mut u64,
    ) -> i32;
}

/// Our own stack, right now — *not* `-[NSException callStackSymbols]` (see the module doc comment:
/// unpopulated once a custom preprocessor is installed). Safe to call here (ordinary thread context,
/// well before any fault — nothing like the async-signal-safety rules past `EXC_PORT` apply).
fn capture_backtrace() -> Vec<String> {
    // A corrupted/cyclic unwind context looping forever is the only thing this guards against — real
    // stacks never get remotely close to it, so it's a safety backstop, not a deliberate cap.
    const FRAME_CEILING: usize = 1 << 16;

    let mut ctx = UnwContext([0; 1336]);
    let mut cursor = UnwCursor([0; 1632]);
    if unsafe { unw_getcontext(&mut ctx) } != 0 {
        return Vec::new();
    }
    if unsafe { unw_init_local(&mut cursor, &mut ctx) } != 0 {
        return Vec::new();
    }

    let mut frames = Vec::new();
    let mut name_buf = [0u8; 512];
    loop {
        let mut pc: u64 = 0;
        if unsafe { unw_get_reg(&mut cursor, UNW_REG_IP, &mut pc) } != 0 {
            break;
        }
        let mut offset: u64 = 0;
        let name = if unsafe {
            unw_get_proc_name(
                &mut cursor,
                name_buf.as_mut_ptr().cast(),
                name_buf.len(),
                &mut offset,
            )
        } == 0
        {
            let end = name_buf.iter().position(|&b| b == 0).unwrap_or(name_buf.len());
            String::from_utf8_lossy(&name_buf[..end]).into_owned()
        } else {
            "??".to_owned()
        };
        frames.push(format!("{pc:#018x}  {name} + {offset:#x}"));

        if frames.len() >= FRAME_CEILING {
            break;
        }
        match unsafe { unw_step(&mut cursor) } {
            n if n > 0 => {}
            _ => break,
        }
    }
    frames
}

/// Logs + stashes an `NSException` for the crash helper to embed as a minidump stream
/// (`crate::nsexception`).
fn capture_nsexception(exception: &NSException) {
    let name = exception.name();
    let reason = exception
        .reason()
        .map_or_else(String::new, |r| r.to_string());
    let frames = capture_backtrace();
    log::error!("Uncaught NSException '{name}': {reason}");
    for frame in frames.iter() {
        log::error!("{frame}")
    }
    crate::nsexception::set(&crate::nsexception::render(&crate::nsexception::NsException {
        name: name.to_string(),
        reason,
        frames,
    }));
}

/// Test hook (the hidden Debugging tab's "NSException" crash kind, or `DTB_KE_CRASH_TEST=nsexception`):
/// raises a real `NSException` with `-raise`. `install_exception_preprocessor` above should capture it
/// (check the session log for "Uncaught NSException") on **either** thread the crash-test harness
/// offers — unlike the hooks this module used to rely on, `objc_setExceptionPreprocessor` doesn't care
/// whether anything downstream ever catches it. Whether this also leaves behind a minidump depends
/// entirely on what happens *after* that: on the main thread AppKit's own (untouched) guarded dispatch
/// may or may not decide to trap in a way our Mach exception port below catches; on a worker thread —
/// or if nothing catches it on the main thread either — it just becomes a plain, uncaptured `abort()`,
/// same as `Kind::Abort` itself.
pub(crate) fn simulate_uncaught_nsexception() {
    let name = objc2_foundation::NSString::from_str("NSInternalInconsistencyException");
    let reason = objc2_foundation::NSString::from_str(
        "DTB_KE_CRASH_TEST: deliberate NSException, to verify the crash-report pipeline captures it",
    );
    // SAFETY: a name and reason are the only required arguments; `userInfo: None` is documented as
    // valid.
    let exception =
        unsafe { NSException::exceptionWithName_reason_userInfo(&name, Some(&reason), None) };
    // `-raise` isn't in this version of `objc2-foundation`'s `NSException` bindings (its
    // "NSExceptionRaisingConveniences" category translated to an empty block) — a plain, statically
    // typed `msg_send!` reaches it exactly like a bound method would.
    unsafe {
        let _: () = objc2::msg_send![&*exception, raise];
    }
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
/// code[2] at 68. Parses a genuine kernel-delivered message and hands off to
/// `handle_exception` — the handler thread doing the receiving *is* the
/// "handler thread" in the usual sense here, unlike `on_fatal_signal`'s
/// synthetic call below.
unsafe fn handle(msg: &[u8]) {
    let thread_port: mach_port_t = rd_u32(msg, 28);
    let task_port: mach_port_t = rd_u32(msg, 40);
    let exception = rd_i32(msg, 60) as u32;
    let code0 = rd_i64(msg, 68) as u64;
    let code1 = rd_i64(msg, 76) as u64;
    handle_exception(
        task_port,
        thread_port,
        mach2::mach_init::mach_thread_self(),
        exception,
        code0,
        code1,
    );
}

/// The shared core: given an exception already reduced to its
/// task/thread/handler-thread ports + kind/code/subcode — whether parsed from
/// a real Mach exception message (`handle` above) or synthesized ourselves
/// (`on_fatal_signal`) — fork the helper, hand it the ports, wait for it to
/// finish. Never returns to a normal caller; both call sites `die()`
/// immediately after.
unsafe fn handle_exception(
    task_port: mach_port_t,
    thread_port: mach_port_t,
    handler_thread: mach_port_t,
    exception: u32,
    code0: u64,
    code1: u64,
) {
    let f = fns();

    dtb_ke_log::fault!("------------[ cut here ]------------");
    crate::oops_art();
    if crate::panic::panicking() {
        dtb_ke_log::fault!("Rust panic! Trapping to an out-of-process minidump");
    } else if exception == EXC_SOFTWARE && code0 == u64::from(EXC_SOFT_SIGNAL) {
        // Our own synthesized exception (`on_fatal_signal`) — code1 is the real signal number.
        match code1 as i32 {
            libc::SIGABRT => dtb_ke_log::fault!("abort() (SIGABRT), capturing a minidump"),
            libc::SIGSYS => dtb_ke_log::fault!("SIGSYS, capturing a minidump"),
            libc::SIGXCPU => dtb_ke_log::fault!("SIGXCPU (CPU time limit), capturing a minidump"),
            libc::SIGXFSZ => dtb_ke_log::fault!("SIGXFSZ (file size limit), capturing a minidump"),
            libc::SIGEMT => dtb_ke_log::fault!("SIGEMT, capturing a minidump"),
            libc::SIGPIPE => dtb_ke_log::fault!("SIGPIPE, capturing a minidump"),
            _ => dtb_ke_log::fault!("signal {code1}, capturing a minidump"),
        }
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
    // Fields: dump_dir \0 slug \0 bootstrap_name \0 main_exe \0 panic_msg \0 build_info \0 nsexception.
    (f.close)(pfd[0]);
    write_cstr(pfd[1], DUMP_DIR.as_ptr());
    write_cstr(pfd[1], APP_SLUG.as_ptr());
    write_cstr(pfd[1], BOOTSTRAP_NAME.as_ptr());
    write_cstr(pfd[1], MAIN_EXE.as_ptr());
    // The panic message static is a NUL-filled buffer when not panicking, so this
    // is just an empty field for a hardware fault.
    write_cstr(pfd[1], crate::panic::message_ptr());
    write_cstr(pfd[1], crate::buildinfo::cstr_ptr());
    // Empty unless `on_uncaught_exception` ran first — i.e. this fault is the trap AppKit raises
    // right after logging an uncaught `NSException`, not a hardware fault or a Rust panic.
    write_cstr(pfd[1], crate::nsexception::cstr_ptr());
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
    fwd.handler_thread = mach_msg_port_descriptor_t::new(handler_thread, MACH_MSG_TYPE_COPY_SEND);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backtrace_capture_is_non_empty() {
        // `callStackSymbols` looked fine in isolation too, until tested from inside the actual
        // preprocessor context — so this asserts on the real captured content, not just "no panic".
        let frames = capture_backtrace();
        assert!(!frames.is_empty(), "capture_backtrace() returned no frames");
        assert!(
            frames.iter().any(|f| f.contains("backtrace_capture_is_non_empty")),
            "expected this test's own frame in the capture, got: {frames:#?}"
        );
    }
}
