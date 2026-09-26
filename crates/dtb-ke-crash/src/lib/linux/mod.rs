//! Linux crash capture — the *thin* half.
//!
//! `install()` (clean context): resolve `syscall(2)` via `dlsym`, create the
//! three pipes the crash path needs (params, "done", watchdog — the process can
//! only crash once, so one set each), ask Yama to let a non-descendant ptrace us
//! (`PR_SET_PTRACER_ANY`), install an alternate signal stack, spawn a watchdog
//! thread, and point `sigaction` for every fatal signal at [`handler`].
//!
//! On a fault [`handler`] (signal context — only `syscall` through the resolved
//! pointer, no libc, no `malloc`):
//! 1. arm the watchdog,
//! 2. extract the embedded helper to `$TMPDIR` (`openat`/`write`/`fchmod`),
//! 3. raw `clone` (a bare `fork`, no `pthread_atfork` handlers); the child
//!    `dup3`s the param pipe onto stdin, clears `CLOEXEC` on the "done" pipe,
//!    and `execve`s the helper with a bare argv,
//! 4. write the helper's parameters — dump dir, slug, main exe, pid, tid, signo,
//!    `si_code`, `si_addr`, the "done" fd, the panic message, then the raw
//!    `ucontext_t` bytes — down the param pipe and close it,
//! 5. `read` the "done" pipe (the helper writes one byte the moment its dump is
//!    captured; EOF if it died early), then `die(134)` — a raw `exit_group`, so
//!    the crashed process and its frozen windows don't linger.
//!
//! `minidump-writer` (`ptrace`), the reporter relaunch, and everything
//! analytical run in the helper. A 12 s watchdog thread is the absolute backstop.

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use libc::{c_int, c_long, c_void};

/// Signals we take over. `SIGILL`/`SIGTRAP` cover the panic-hook trap
/// (`ud2` / `brk`), the rest are the usual hardware faults + `abort`.
const FATAL_SIGNALS: [c_int; 6] = [
    libc::SIGSEGV,
    libc::SIGBUS,
    libc::SIGABRT,
    libc::SIGILL,
    libc::SIGFPE,
    libc::SIGTRAP,
];

/// How many bytes at `uctx` we forward to the helper: exactly one
/// `ucontext_t`. The kernel wrote a `struct ucontext` into the signal frame, so
/// this stays inside the frame (and the alt stack). The integer register save
/// area — all `minidump-writer` reads — is at the front; the FP area is not
/// forwarded (the helper zero-fills `float_state`).
const UCONTEXT_BYTES: usize = core::mem::size_of::<libc::ucontext_t>();

/// Watchdog grace period after a fault before it force-exits.
const WATCHDOG_SECS: u64 = 12;

static HELPER_BYTES: &[u8] = include_bytes!(env!("DTB_KE_CRASH_HELPER"));

static HANDLED: AtomicBool = AtomicBool::new(false);

// ── module state, all set in install() ─────────────────────────────────

static mut HELPER_PATH: [u8; 1024] = [0; 1024];
static mut DUMP_DIR: [u8; 1024] = [0; 1024];
static mut APP_SLUG: [u8; 64] = [0; 64];
static mut MAIN_EXE: [u8; 1024] = [0; 1024];
static mut PARAM_FDS: [c_int; 2] = [-1, -1];
static mut DONE_FDS: [c_int; 2] = [-1, -1];
static mut WATCHDOG_FDS: [c_int; 2] = [-1, -1];
static mut ALT_STACK: [u8; 64 * 1024] = [0; 64 * 1024];

/// `dlsym`'d `syscall(2)` — the single entry point for every crash-path OS call,
/// resolved while the dynamic linker's lock is uncontended so the crash path
/// never triggers lazy symbol resolution.
static mut SYSCALL: Option<unsafe extern "C" fn(c_long, ...) -> c_long> = None;

unsafe extern "C" {
    static environ: *const *const libc::c_char;
}

/// Invoke a syscall through the resolved `syscall(2)` pointer.
macro_rules! sys {
    ($n:expr $(, $a:expr)* $(,)?) => {
        (SYSCALL.unwrap_unchecked())($n as c_long $(, $a as c_long)*)
    };
}

// ── install ────────────────────────────────────────────────────────────

pub(crate) fn install(dump_dir: &Path, app_slug: &str) -> Result<(), crate::library::InstallError> {
    use crate::library::InstallError;

    if HELPER_BYTES.is_empty() {
        return Err(InstallError::NoHelper);
    }

    unsafe {
        put_cstr(&mut DUMP_DIR, dump_dir.to_string_lossy().as_bytes());
        put_cstr(&mut APP_SLUG, app_slug.as_bytes());
        let tmp = std::env::temp_dir().join(crate::HELPER_FILE_NAME);
        put_cstr(&mut HELPER_PATH, tmp.to_string_lossy().as_bytes());
        if let Ok(exe) = std::env::current_exe() {
            put_cstr(&mut MAIN_EXE, exe.to_string_lossy().as_bytes());
        }

        // Resolve syscall(2) once, now.
        let p = libc::dlsym(libc::RTLD_DEFAULT, c"syscall".as_ptr());
        if p.is_null() {
            return Err(InstallError::Os("dlsym(syscall) failed".into()));
        }
        SYSCALL = Some(core::mem::transmute::<
            *mut c_void,
            unsafe extern "C" fn(c_long, ...) -> c_long,
        >(p));

        // The three pipes the crash path needs. CLOEXEC so an unrelated
        // fork/exec elsewhere in the app never leaks them.
        if libc::pipe2((&raw mut PARAM_FDS).cast(), libc::O_CLOEXEC) != 0
            || libc::pipe2((&raw mut DONE_FDS).cast(), libc::O_CLOEXEC) != 0
            || libc::pipe2((&raw mut WATCHDOG_FDS).cast(), libc::O_CLOEXEC) != 0
        {
            return Err(InstallError::Os("pipe2 failed".into()));
        }

        // Let the on-demand helper (not a descendant of ours) ptrace us.
        // Best-effort: harmless on kernels without Yama.
        libc::prctl(libc::PR_SET_PTRACER, libc::PR_SET_PTRACER_ANY, 0, 0, 0);

        // Alt stack so a stack-overflow SIGSEGV still runs the handler.
        let ss = libc::stack_t {
            ss_sp: (&raw mut ALT_STACK).cast(),
            ss_flags: 0,
            ss_size: ALT_STACK.len(),
        };
        if libc::sigaltstack(&ss, std::ptr::null_mut()) != 0 {
            return Err(InstallError::Os("sigaltstack failed".into()));
        }

        std::thread::Builder::new()
            .name("dtbke-crash-watchdog".into())
            .stack_size(64 * 1024)
            .spawn(|| unsafe {
                let mut b = [0u8; 1];
                libc::read(WATCHDOG_FDS[0], b.as_mut_ptr().cast(), 1);
                std::thread::sleep(std::time::Duration::from_secs(WATCHDOG_SECS));
                die(134);
            })
            .map_err(|e| InstallError::Os(e.to_string()))?;

        let mut act: libc::sigaction = std::mem::zeroed();
        act.sa_sigaction = handler as *const () as usize;
        act.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
        libc::sigemptyset(&mut act.sa_mask);
        for s in FATAL_SIGNALS {
            libc::sigaddset(&mut act.sa_mask, s);
        }
        for s in FATAL_SIGNALS {
            if libc::sigaction(s, &act, std::ptr::null_mut()) != 0 {
                return Err(InstallError::Os(format!("sigaction({s}) failed")));
            }
        }
    }
    Ok(())
}

// ── the signal handler ─────────────────────────────────────────────────

extern "C" fn handler(sig: c_int, info: *mut libc::siginfo_t, uctx: *mut c_void) {
    if HANDLED.swap(true, Ordering::SeqCst) {
        die(134);
    }
    unsafe {
        // Arm the watchdog.
        let one = [1u8];
        sys!(libc::SYS_write, WATCHDOG_FDS[1], one.as_ptr(), 1usize);

        let (si_code, si_addr) = if info.is_null() {
            (0i64, 0u64)
        } else {
            // `si_addr` is only a fault address for these; for SIGABRT / SIGTRAP
            // it aliases other union members, so don't forward garbage.
            let addr = if matches!(
                sig,
                libc::SIGSEGV | libc::SIGBUS | libc::SIGILL | libc::SIGFPE
            ) {
                (*info).si_addr() as usize as u64
            } else {
                0
            };
            ((*info).si_code as i64, addr)
        };

        dtb_ke_log::fault!("------------[ cut here ]------------");
        crate::oops_art();
        if crate::panic::panicking() {
            dtb_ke_log::fault!("Rust panic, trapping to an out-of-process minidump");
        } else {
            dtb_ke_log::fault!(
                "Caught signal {sig} (si_code {si_code}) at {si_addr:#x}, capturing a minidump"
            );
        }

        extract_helper();

        // Bare fork — no pthread_atfork handlers (they can take locks the
        // crashed thread may hold).
        let child = sys!(libc::SYS_clone, libc::SIGCHLD, 0, 0, 0, 0) as libc::pid_t;
        if child == 0 {
            // ── child ──
            sys!(libc::SYS_dup3, PARAM_FDS[0], 0, 0); // params on stdin
            sys!(libc::SYS_fcntl, DONE_FDS[1], libc::F_SETFD, 0); // survive execve
            let argv = [
                HELPER_PATH.as_ptr().cast::<libc::c_char>(),
                core::ptr::null(),
            ];
            sys!(
                libc::SYS_execve,
                HELPER_PATH.as_ptr(),
                argv.as_ptr(),
                environ
            );
            die(127);
        }
        if child < 0 {
            dtb_ke_log::fault!("Could not fork the crash helper, no minidump");
            dtb_ke_log::fault!(" ---[ end trace {si_addr:#x} ]---");
            die(134);
        }

        // ── parent ──
        sys!(libc::SYS_close, PARAM_FDS[0]);
        sys!(libc::SYS_close, DONE_FDS[1]);

        // params: dump_dir \0 slug \0 main_exe \0 pid \0 tid \0 signo \0
        //         si_code \0 si_addr \0 done_fd \0 panic_msg \0 build_info \0 <ucontext bytes>
        let w = PARAM_FDS[1];
        write_cstr(w, DUMP_DIR.as_ptr());
        write_cstr(w, APP_SLUG.as_ptr());
        write_cstr(w, MAIN_EXE.as_ptr());
        write_uint(w, sys!(libc::SYS_getpid) as u64);
        write_uint(w, sys!(libc::SYS_gettid) as u64);
        write_uint(w, sig as u64);
        write_int(w, si_code);
        write_uint(w, si_addr);
        write_uint(w, DONE_FDS[1] as u64); // fd number is valid in the child
        write_cstr(w, crate::panic::message_ptr());
        write_cstr(w, crate::buildinfo::cstr_ptr());
        write_all(w, uctx.cast::<u8>(), UCONTEXT_BYTES);
        sys!(libc::SYS_close, w);

        // Wait (watchdog-bounded) for the helper's "done" byte / EOF.
        let mut b = [0u8; 1];
        sys!(libc::SYS_read, DONE_FDS[0], b.as_mut_ptr(), 1usize);

        die(134);
    }
}

/// Terminate the whole process *now* via a raw `exit_group`, touching no PLT
/// entry (mirrors the macOS `die`).
#[inline(always)]
pub(crate) fn die(code: i32) -> ! {
    unsafe {
        if let Some(f) = SYSCALL {
            f(libc::SYS_exit_group, code as c_long);
        }
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!("syscall", in("rax") 231usize, in("rdi") code, options(noreturn, nostack));
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!("svc #0", in("x8") 94usize, in("x0") code, options(noreturn, nostack));
        #[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
        libc::_exit(code)
    }
}

// ── crash-path helpers (syscall only) ──────────────────────────────────

unsafe fn extract_helper() {
    // openat(AT_FDCWD, path, O_CREAT|O_TRUNC|O_WRONLY, 0700)
    let fd = sys!(
        libc::SYS_openat,
        libc::AT_FDCWD,
        HELPER_PATH.as_ptr(),
        libc::O_CREAT | libc::O_TRUNC | libc::O_WRONLY,
        0o700
    ) as c_int;
    if fd < 0 {
        return;
    }
    let mut off = 0usize;
    while off < HELPER_BYTES.len() {
        let n = sys!(
            libc::SYS_write,
            fd,
            HELPER_BYTES.as_ptr().add(off),
            HELPER_BYTES.len() - off
        );
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
    sys!(libc::SYS_fchmod, fd, 0o700);
    sys!(libc::SYS_close, fd);
}

unsafe fn write_all(fd: c_int, p: *const u8, len: usize) {
    let mut off = 0usize;
    while off < len {
        let n = sys!(libc::SYS_write, fd, p.add(off), len - off);
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

/// Write a NUL-terminated C string + its terminating NUL (the field separator).
unsafe fn write_cstr(fd: c_int, p: *const u8) {
    let mut len = 0usize;
    while *p.add(len) != 0 {
        len += 1;
    }
    write_all(fd, p, len + 1);
}

unsafe fn write_uint(fd: c_int, mut v: u64) {
    let mut buf = [0u8; 21];
    let mut i = buf.len() - 1; // leave the last byte as the NUL separator
    loop {
        i -= 1;
        buf[i] = b'0' + (v % 10) as u8;
        v /= 10;
        if v == 0 {
            break;
        }
    }
    write_all(fd, buf.as_ptr().add(i), buf.len() - i);
}

unsafe fn write_int(fd: c_int, v: i64) {
    if v < 0 {
        write_all(fd, b"-".as_ptr(), 1);
        write_uint(fd, v.unsigned_abs());
    } else {
        write_uint(fd, v as u64);
    }
}

unsafe fn put_cstr(dst: &mut [u8], src: &[u8]) {
    let n = src.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&src[..n]);
    dst[n] = 0;
}
