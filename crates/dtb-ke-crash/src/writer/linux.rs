//! Linux out-of-process capture: `ptrace` (via `minidump-writer`) against the crashed pid/tid.

use std::io::Cursor;

use minidump_writer::CrashContextExt;
use minidump_writer::minidump_writer::MinidumpWriterConfig;

/// Reads the crashed process's memory via `ptrace` (`minidump-writer`) and returns minidump bytes —
/// pure, no spawn/exec at all. Called directly by `dtb-ke-ui`'s self-relaunched capture path
/// (`crash_report::run_linux_capture`).
pub fn capture(pid: i32, tid: i32, signo: u32, si_code: i32, si_addr: u64, uctx: &[u8]) -> Option<Vec<u8>> {
    let blamed = if tid != 0 { tid } else { pid };

    // Rebuild the signal `CrashContext`. `minidump-writer` reads the integer
    // registers from `context.uc_mcontext` (front of the struct, identical
    // ABI on both libc flavours) and the exception fields from `siginfo`;
    // `float_state` is not forwarded.
    let mut cc: crash_context::CrashContext = unsafe { std::mem::zeroed() };
    let n = uctx.len().min(std::mem::size_of::<crash_context::ucontext_t>());
    unsafe {
        std::ptr::copy_nonoverlapping(uctx.as_ptr(), std::ptr::from_mut(&mut cc.context).cast::<u8>(), n);
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
