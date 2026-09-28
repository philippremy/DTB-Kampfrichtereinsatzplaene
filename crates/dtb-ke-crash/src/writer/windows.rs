//! Windows out-of-process capture: `OpenProcess` + `MiniDumpWriteDump(ClientPointers=TRUE)` against
//! the crashed pid.

use crash_context::CrashContext;
use minidump_writer::minidump_writer::MinidumpWriter;

/// Reads the crashed process's memory via `OpenProcess` + `MiniDumpWriteDump(ClientPointers=TRUE)`
/// and returns minidump bytes — pure, no spawn/exec at all. Called directly by `dtb-ke-ui`'s
/// self-relaunched capture path (`crash_report::run_windows_capture`).
pub fn capture(pid: u32, tid: u32, exc_ptr: usize, exc_code: i32, slug: &str) -> Option<Vec<u8>> {
    // `dump_crash_context` writes to a `File`; dump to a temp, read it back.
    let tmp = std::env::temp_dir().join(format!("{slug}-{pid}.{}.part", crate::DUMP_EXTENSION));
    let mut file = std::fs::File::create(&tmp).ok()?;
    let cc = CrashContext { exception_pointers: exc_ptr as *const _, exception_code: exc_code, process_id: pid, thread_id: tid };
    let res = MinidumpWriter::dump_crash_context(&cc, None, &mut file);
    let _ = file.sync_all();
    drop(file);
    let bytes = std::fs::read(&tmp).ok();
    let _ = std::fs::remove_file(&tmp);
    res.ok()?;
    bytes.filter(|b| !b.is_empty())
}
