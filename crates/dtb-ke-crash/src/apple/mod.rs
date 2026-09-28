//! Shared Apple (macOS + iOS, arm64) pieces of the in-process crash path: the signal-safe crash-record
//! writer ([`capture`]) and the launch-time session snapshot ([`session`]). The iOS handler
//! (`ios/`) drives them; on macOS they exist so the same code can be tested against
//! `minidump-writer` (`tests/apple_capture.rs`).

pub mod capture;
pub mod session;

/// Terminate the whole process *now*, bypassing libc (a raw `SYS_exit`), for the watchdog path — the
/// same reasoning as `macos::die`: a lazy-bound `_exit` stub can deadlock on dyld's lock.
#[inline(always)]
pub fn die(code: i32) -> ! {
    unsafe {
        core::arch::asm!(
            "mov x16, #1",
            "svc #0x80",
            in("x0") code,
            options(noreturn, nostack),
        );
    }
}
