//! Every deliberate crash/fault the Debugging tab (`settings_window::debug_tab`) and
//! `DTB_KE_CRASH_TEST` can raise, as one enum: [`Fault::SCENARIOS`] are shown as their own row
//! each (a Rust panic, a stack overflow, …), [`Fault::SIGNALS`] are the literal OS signals our
//! crash handler is expected to catch, picked from a dropdown. See `dtb-ke-crash`'s
//! `lib::{macos,ios,linux}` modules for exactly which signals each platform actually installs a
//! handler for — [`Fault::available`] mirrors that.
//!
//! [`Fault::trigger`] is the "make it happen, right now, on this thread" call; `Fault::Borrow` is
//! the one exception — it needs a live gpui `Entity` mid-`update`, which only exists with an
//! `&mut App` in hand, so `crate::debug::crash::trigger` special-cases it instead of calling
//! `trigger()` for it.

use gpui_kit::SharedString;

use crate::i18n::{ActiveLocale, Locale};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Fault {
    /// A Rust panic (the hook traps to the OS exception path).
    Panic,
    /// Reading a gpui entity from inside its own `update` — the panic class the drag-reorder work hit.
    Borrow,
    /// Unbounded recursion.
    StackOverflow,
    /// A real, deliberately unhandled `NSException` (macOS/iOS only). See
    /// `dtb_ke_crash::simulate_uncaught_nsexception`'s doc comment for what this does and doesn't verify.
    NSException,
    /// A write through a null pointer (`EXC_BAD_ACCESS` / `SIGSEGV`).
    Segv,
    /// Touches an `mmap`ed page past its now-truncated backing file (`EXC_BAD_ACCESS` / `SIGBUS`).
    Bus,
    /// A genuinely undefined machine instruction (`EXC_BAD_INSTRUCTION` / `SIGILL`).
    Ill,
    /// A real breakpoint instruction (`EXC_BREAKPOINT` / `SIGTRAP`).
    Trap,
    /// Integer division by zero via a raw processor instruction (`EXC_ARITHMETIC` / `SIGFPE`).
    Fpe,
    /// `std::process::abort()` (`SIGABRT`).
    Abort,
    /// An invalid raw system call.
    Sys,
    /// An exceeded CPU time limit.
    Xcpu,
    /// A write past an imposed file-size limit.
    Xfsz,
    /// Sent directly — no real cause exists on modern hardware (macOS/iOS only).
    Emt,
    /// A write to a pipe whose read end is already closed.
    Pipe,
}

impl Fault {
    /// Shown as their own settings row — a scenario, not "raise this literal signal".
    pub const SCENARIOS: [Self; 4] = [Self::Panic, Self::Borrow, Self::StackOverflow, Self::NSException];

    /// The literal OS signals — picked from the Debugging tab's signal dropdown.
    pub const SIGNALS: [Self; 11] = [
        Self::Segv,
        Self::Bus,
        Self::Ill,
        Self::Trap,
        Self::Fpe,
        Self::Abort,
        Self::Sys,
        Self::Xcpu,
        Self::Xfsz,
        Self::Emt,
        Self::Pipe,
    ];

    /// The catalog key stem: `settings.debug.fault-<stem>-title` / `-description`.
    fn stem(self) -> &'static str {
        match self {
            Self::Panic => "panic",
            Self::Borrow => "borrow",
            Self::StackOverflow => "overflow",
            Self::NSException => "nsexception",
            Self::Segv => "segv",
            Self::Bus => "bus",
            Self::Ill => "ill",
            Self::Trap => "trap",
            Self::Fpe => "fpe",
            Self::Abort => "abort",
            Self::Sys => "sys",
            Self::Xcpu => "xcpu",
            Self::Xfsz => "xfsz",
            Self::Emt => "emt",
            Self::Pipe => "pipe",
        }
    }

    pub fn label(self, locale: &Locale) -> SharedString {
        locale.t(&format!("settings.debug.fault-{}-title", self.stem()))
    }

    pub fn description(self, locale: &Locale) -> SharedString {
        locale.t(&format!("settings.debug.fault-{}-description", self.stem()))
    }

    /// Whether this fault can actually be raised on the running platform/architecture. `Err`
    /// carries a localized, user-facing sentence — meant straight for a disabled trigger button's
    /// tooltip, not for logging.
    pub fn available(self, locale: &Locale) -> Result<(), String> {
        let reason = |key: &str| Err(locale.t(&format!("settings.debug.unavailable-{key}")).to_string());
        let is_signal = Self::SIGNALS.contains(&self);
        if is_signal && cfg!(target_os = "windows") {
            // Every `SIGNALS` entry models a POSIX signal our crash handler catches via
            // `sigaction`/a Mach exception port — neither exists on Windows, which instead catches
            // unhandled SEH exceptions process-wide (`SetUnhandledExceptionFilter`, see
            // `dtb-ke-crash`'s `lib/windows/mod.rs`). None of the triggers in this module are
            // meaningful there, checked first so it takes precedence over the arch-specific reasons
            // below on a hypothetical non-x86_64 Windows build.
            return reason("windows");
        }
        match self {
            Self::NSException if !cfg!(any(target_os = "macos", target_os = "ios")) => reason("apple-only"),
            Self::Emt if !cfg!(any(target_os = "macos", target_os = "ios")) => reason("emt-platform"),
            Self::Fpe if !cfg!(target_arch = "x86_64") => reason("fpe-arch"),
            _ => Ok(()),
        }
    }

    /// Raises this fault right now, on the calling thread. Does not return (the process ends up
    /// crashing) — except [`Self::Borrow`], which is a documented no-op here; see the module doc
    /// comment.
    pub fn trigger(self) {
        match self {
            Self::Panic => panic!("DTB_KE_CRASH_TEST: deliberate panic"),
            Self::Borrow => {}
            Self::StackOverflow => {
                std::hint::black_box(overflow(0));
            }
            Self::NSException => ns_exception(),
            Self::Segv => segv(),
            Self::Bus => bus(),
            Self::Ill => illegal_instruction(),
            Self::Trap => breakpoint(),
            Self::Fpe => divide_by_zero(),
            Self::Abort => std::process::abort(),
            Self::Sys => bad_syscall(),
            Self::Xcpu => cpu_time_limit(),
            Self::Xfsz => file_size_limit(),
            Self::Emt => emt(),
            Self::Pipe => broken_pipe(),
        }
    }
}

// Unbounded recursion is the point.
#[allow(unconditional_recursion)]
#[inline(never)]
fn overflow(depth: u64) -> u64 {
    let pad = [depth as u8; 1024];
    std::hint::black_box(&pad);
    overflow(depth + 1) + u64::from(pad[0])
}

fn ns_exception() {
    #[cfg(target_os = "macos")]
    dtb_ke_crash::simulate_uncaught_nsexception();
    #[cfg(not(target_os = "macos"))]
    panic!("DTB_KE_CRASH_TEST: NSException is macOS-only; simulating with a panic instead");
}

#[allow(clippy::manual_dangling_ptr)]
fn segv() {
    unsafe { std::ptr::null_mut::<u64>().write_volatile(0xdead) }
}

/// Touches an `mmap`ed page past the end of its now-truncated backing file — a real `SIGBUS` on
/// every platform that has `mmap` at all (unlike a raw unaligned-address write, which is only a
/// `SIGBUS` on Darwin; on Linux the same trick just faults `SIGSEGV`, since address 1 there is
/// simply unmapped, not misaligned).
#[cfg(not(target_os = "windows"))]
fn bus() {
    use std::os::fd::AsRawFd;
    let path = std::env::temp_dir().join(format!("dtb-ke-crash-test-{}", std::process::id()));
    let file = match std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(true).open(&path) {
        Ok(f) => f,
        Err(err) => {
            log::error!("crash simulation: cannot create the bus-error test file: {err}");
            std::process::abort();
        }
    };
    if file.set_len(4096).is_err() {
        std::process::abort();
    }
    let ptr = unsafe {
        libc::mmap(std::ptr::null_mut(), 4096, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, file.as_raw_fd(), 0)
    };
    let _ = std::fs::remove_file(&path);
    if ptr == libc::MAP_FAILED {
        log::error!("crash simulation: mmap failed for the bus-error test");
        std::process::abort();
    }
    // Shrinking the file out from under the still-live mapping is what makes touching it fault.
    if file.set_len(0).is_err() {
        std::process::abort();
    }
    unsafe { (ptr as *mut u8).write_volatile(1) };
}
#[cfg(target_os = "windows")]
fn bus() {
    std::process::abort();
}

#[cfg(target_arch = "aarch64")]
fn illegal_instruction() {
    unsafe { core::arch::asm!("udf #0", options(noreturn)) }
}
#[cfg(target_arch = "x86_64")]
fn illegal_instruction() {
    unsafe { core::arch::asm!("ud2", options(noreturn)) }
}
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn illegal_instruction() {
    std::process::abort();
}

// A plain `brk #0` / `int3` — distinct from `dtb_ke_crash`'s own panic trap (`brk #0xdb` /
// `ud2`), so a real breakpoint test never gets mistaken for a panic by anything that inspects the
// trap's immediate later.
#[cfg(target_arch = "aarch64")]
fn breakpoint() {
    unsafe { core::arch::asm!("brk #0", options(noreturn)) }
}
#[cfg(target_arch = "x86_64")]
fn breakpoint() {
    unsafe { core::arch::asm!("int3", options(noreturn)) }
}
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
fn breakpoint() {
    std::process::abort();
}

/// A raw `div` by zero — bypasses Rust's own checked-arithmetic panic, which would otherwise
/// intercept this before it ever reaches the CPU. Only x86_64 hardware traps on this (`#DE`);
/// AArch64's `SDIV`/`UDIV` are defined to return 0 for a zero divisor instead of faulting, and
/// unmasked IEEE-754 FP traps aren't implemented on Apple Silicon's FPU either — there is no
/// reliable way to raise a genuine `SIGFPE` there (see [`Fault::available`]).
#[cfg(target_arch = "x86_64")]
fn divide_by_zero() {
    unsafe {
        core::arch::asm!("xor eax, eax", "xor edx, edx", "xor ecx, ecx", "div ecx", options(noreturn))
    }
}
#[cfg(not(target_arch = "x86_64"))]
fn divide_by_zero() {
    std::process::abort();
}

/// An unrecognized raw syscall number traps `SIGSYS` on Darwin (BSD heritage) — a genuinely
/// invalid system call, not a synthetic signal. Linux's `syscall(2)` just returns `ENOSYS` for an
/// unknown number instead, so there `raise` is the only way to exercise the handler.
#[cfg(any(target_os = "macos", target_os = "ios"))]
fn bad_syscall() {
    unsafe { libc::syscall(0x7fff_ffff) };
    unsafe { libc::raise(libc::SIGSYS) };
}
#[cfg(target_os = "linux")]
fn bad_syscall() {
    unsafe { libc::raise(libc::SIGSYS) };
}
#[cfg(target_os = "windows")]
fn bad_syscall() {
    std::process::abort();
}

#[cfg(not(target_os = "windows"))]
fn cpu_time_limit() {
    unsafe { libc::raise(libc::SIGXCPU) };
}
#[cfg(target_os = "windows")]
fn cpu_time_limit() {
    std::process::abort();
}

/// A real `setrlimit(RLIMIT_FSIZE, …)` + a write past it — a genuine `SIGXFSZ` from the kernel,
/// not a raised one.
#[cfg(not(target_os = "windows"))]
fn file_size_limit() {
    use std::io::Write;
    let path = std::env::temp_dir().join(format!("dtb-ke-crash-test-xfsz-{}", std::process::id()));
    let mut file = match std::fs::OpenOptions::new().write(true).create(true).truncate(true).open(&path) {
        Ok(f) => f,
        Err(err) => {
            log::error!("crash simulation: cannot create the file-size test file: {err}");
            std::process::abort();
        }
    };
    let _ = std::fs::remove_file(&path);
    let limit = libc::rlimit { rlim_cur: 1, rlim_max: 1 };
    if unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &limit) } != 0 {
        log::error!("crash simulation: setrlimit(RLIMIT_FSIZE) failed");
        std::process::abort();
    }
    let _ = file.write_all(&[0u8; 4096]);
    std::process::abort(); // only reached if the write above somehow didn't fault
}
#[cfg(target_os = "windows")]
fn file_size_limit() {
    std::process::abort();
}

#[cfg(any(target_os = "macos", target_os = "ios"))]
fn emt() {
    unsafe { libc::raise(libc::SIGEMT) };
}
#[cfg(not(any(target_os = "macos", target_os = "ios")))]
fn emt() {
    std::process::abort();
}

/// A pipe with its read end already closed — a genuine `SIGPIPE` from the write, not a raised one.
#[cfg(not(target_os = "windows"))]
fn broken_pipe() {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        log::error!("crash simulation: pipe() failed");
        std::process::abort();
    }
    unsafe { libc::close(fds[0]) };
    let byte = [0u8; 1];
    unsafe { libc::write(fds[1], byte.as_ptr().cast(), 1) };
    std::process::abort(); // only reached if the write above somehow didn't fault
}
#[cfg(target_os = "windows")]
fn broken_pipe() {
    std::process::abort();
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every label/description/unavailable-reason text resolves to real text — a missing key
    /// would show as the key itself.
    #[test]
    fn labels_are_all_in_the_catalog() {
        let locale = crate::i18n::test_locale();
        let looks_like_key = |t: &str| t.starts_with("settings.debug.");
        for fault in Fault::SCENARIOS.into_iter().chain(Fault::SIGNALS) {
            assert!(!looks_like_key(&fault.label(&locale)), "{fault:?} label");
            assert!(!looks_like_key(&fault.description(&locale)), "{fault:?} description");
            if let Err(reason) = fault.available(&locale) {
                assert!(!looks_like_key(&reason), "{fault:?} unavailable reason");
            }
        }
    }

    #[test]
    fn scenarios_and_signals_partition_every_fault() {
        assert_eq!(Fault::SCENARIOS.len() + Fault::SIGNALS.len(), 15);
    }
}
