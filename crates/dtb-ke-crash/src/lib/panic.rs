//! Panic → trap bridge.
//!
//! The hook records the formatted message + location into a fixed static (no
//! `Mutex` — the crash handler reads it lock-free), runs the previous hook, then
//! traps so the crash lands on the OS exception path (a Mach exception on macOS,
//! `SetUnhandledExceptionFilter` on Windows). Works with `panic = "abort"` (the
//! hook still runs before `abort`).

use std::sync::atomic::{AtomicUsize, Ordering};

const CAP: usize = 4096;
static mut MESSAGE: [u8; CAP] = [0; CAP];
/// `usize::MAX` while not panicking; otherwise the byte length in `MESSAGE`.
static MESSAGE_LEN: AtomicUsize = AtomicUsize::new(usize::MAX);

pub(crate) fn install_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        // `PanicHookInfo`'s Display is "panicked at <loc>:\n<payload>".
        let mut buf = String::with_capacity(256);
        use std::fmt::Write;
        let _ = write!(buf, "{info}");
        let bytes = buf.as_bytes();
        let n = bytes.len().min(CAP - 1);
        // The reader only looks once MESSAGE_LEN is set (SeqCst); a panic then
        // traps, so there is no concurrent writer.
        unsafe {
            core::ptr::copy_nonoverlapping(bytes.as_ptr(), (&raw mut MESSAGE) as *mut u8, n);
        }
        MESSAGE_LEN.store(n, Ordering::SeqCst);

        previous(info);
        trap();
    }));
}

/// Whether the current crash originated from a Rust panic.
pub(crate) fn panicking() -> bool {
    MESSAGE_LEN.load(Ordering::SeqCst) != usize::MAX
}

/// Pointer to the NUL-terminated panic message (the static is zero-init, so the
/// bytes after the recorded content are `\0`). Only meaningful when
/// [`panicking`] is true. Written down the pipe to the crash helper.
#[cfg(any(target_os = "macos", target_os = "windows", target_os = "linux"))]
pub(crate) fn message_ptr() -> *const u8 {
    (&raw const MESSAGE) as *const u8
}

#[inline(never)]
fn trap() -> ! {
    // Windows: raise an exception that reaches `SetUnhandledExceptionFilter`.
    // (If a stray `__try/__except` on the panic-unwind path ever swallowed this
    // on a real box, a vectored handler would be the fix.)
    #[cfg(target_os = "windows")]
    unsafe {
        const STATUS_BREAKPOINT: u32 = 0x8000_0003;
        const EXCEPTION_NONCONTINUABLE: u32 = 0x1;
        unsafe extern "system" {
            fn RaiseException(code: u32, flags: u32, n_args: u32, args: *const usize);
        }
        RaiseException(
            STATUS_BREAKPOINT,
            EXCEPTION_NONCONTINUABLE,
            0,
            core::ptr::null(),
        );
        core::hint::unreachable_unchecked()
    }
    #[cfg(all(not(target_os = "windows"), target_arch = "aarch64"))]
    unsafe {
        core::arch::asm!("brk #0xdb", options(noreturn))
    }
    #[cfg(all(not(target_os = "windows"), target_arch = "x86_64"))]
    unsafe {
        core::arch::asm!("ud2", options(noreturn))
    }
    #[cfg(not(any(target_os = "windows", target_arch = "aarch64", target_arch = "x86_64")))]
    std::process::abort()
}
