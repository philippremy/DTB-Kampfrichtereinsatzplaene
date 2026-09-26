//! Quick offline look at a `.dmp` (minidump) — crash reason, per-thread stacks
//! (frame-pointer + scan unwind, no symbol resolution), and the module list with
//! debug ids for manual symbolication.
//!
//! ```text
//! dtb-ke-symbolize <dump.dtbkedmp>
//! ```
//!
//! For symbol resolution (`func @ file:line`) use `minidump-stackwalk
//! --symbols-path <dir>` (`cargo install minidump-stackwalk`), or open the
//! `.dmp` in `lldb -c` / WinDbg / Visual Studio.

use std::io::Write;

use minidump::{
    Minidump, MinidumpException, MinidumpModule, MinidumpModuleList, MinidumpSystemInfo,
    MinidumpThreadList, MinidumpThreadNames, Module,
};
use minidump_unwind::{
    CallStack, FrameTrust, MultiSymbolProvider, StackFrame, SystemInfo, walk_stack,
};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: dtb-ke-symbolize <dump.dtbkedmp>");
        std::process::exit(2);
    };
    if path == "-h" || path == "--help" {
        eprintln!(
            "usage: dtb-ke-symbolize <dump.dtbkedmp>\n\nFor symbolication: `minidump-stackwalk --symbols-path <dir> <dump.dtbkedmp>`, or open in lldb -c / WinDbg."
        );
        return;
    }

    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("error: {path}: {e}");
            std::process::exit(1);
        }
    };
    let dump = match Minidump::read(bytes) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: not a valid minidump: {e}");
            std::process::exit(1);
        }
    };

    let mut out = std::io::stdout().lock();
    let _ = report(&mut out, &dump);
}

fn report<W: Write>(w: &mut W, dump: &Minidump<'_, Vec<u8>>) -> std::io::Result<()> {
    let sys = dump.get_stream::<MinidumpSystemInfo>().ok();
    let modules = dump
        .get_stream::<MinidumpModuleList>()
        .unwrap_or_else(|_| MinidumpModuleList::new());
    let threads = dump.get_stream::<MinidumpThreadList>().ok();
    let names = dump.get_stream::<MinidumpThreadNames>().ok();
    let exception = dump.get_stream::<MinidumpException>().ok();
    let memory_list = dump.get_memory().unwrap_or_default();

    let exe = modules
        .main_module()
        .map(|m| basename(&m.code_file()).to_string())
        .unwrap_or_else(|| "?".into());
    writeln!(w, "{exe}")?;

    if let (Some(sys), Some(exc)) = (&sys, &exception) {
        let reason = exc.get_crash_reason(sys.os, sys.cpu);
        let addr = exc.get_crash_address(sys.os, sys.cpu);
        writeln!(w, "  reason:  {reason}")?;
        writeln!(w, "  address: {addr:#018x}")?;
        writeln!(
            w,
            "  thread:  {:#x} (crashed)",
            exc.get_crashing_thread_id()
        )?;
    }
    if let Some(sys) = &sys {
        writeln!(w, "  system:  {:?} / {:?}", sys.os, sys.cpu)?;
    }
    writeln!(w)?;

    let (Some(threads), Some(sys)) = (threads, sys) else {
        writeln!(w, "(no thread list)")?;
        return module_table(w, &modules);
    };

    let unwind_sys = SystemInfo {
        os: sys.os,
        cpu: sys.cpu,
        os_version: None,
        os_build: None,
        cpu_info: None,
        cpu_microcode_version: None,
        cpu_count: 1,
    };
    let provider = MultiSymbolProvider::new();
    let crashing_tid = exception.as_ref().map(|e| e.get_crashing_thread_id());

    let misc = dump.get_stream::<minidump::MinidumpMiscInfo>().ok();

    futures::executor::block_on(async {
        for (idx, thread) in threads.threads.iter().enumerate() {
            let tid = thread.raw.thread_id;
            let name = names
                .as_ref()
                .and_then(|n| n.get_name(tid))
                .map(|c| c.into_owned())
                .unwrap_or_default();
            let marker = if Some(tid) == crashing_tid {
                " (crashed)"
            } else {
                ""
            };
            let label = if name.is_empty() {
                String::new()
            } else {
                format!(" \"{name}\"")
            };
            let _ = writeln!(w, "Thread {idx} [{tid:#x}]{label}{marker}:");

            let Some(context) = thread.context(&sys, misc.as_ref()) else {
                let _ = writeln!(w, "  (no context)\n");
                continue;
            };
            let mut stack = CallStack::with_context(context.into_owned());
            stack.thread_id = tid;
            let stack_memory = thread.stack_memory(&memory_list);

            walk_stack(
                idx,
                (),
                &mut stack,
                stack_memory,
                &modules,
                &unwind_sys,
                &provider,
            )
            .await;

            for (n, frame) in stack.frames.iter().enumerate() {
                let _ = writeln!(w, "  {}", fmt_frame(n, frame));
            }
            let _ = writeln!(w);
        }
    });

    module_table(w, &modules)
}

fn fmt_frame(n: usize, f: &StackFrame) -> String {
    let ip = f.instruction;
    let trust = match f.trust {
        FrameTrust::Context => "ctx",
        FrameTrust::CfiScan | FrameTrust::Scan => "scan",
        FrameTrust::FramePointer => "fp",
        FrameTrust::CallFrameInfo => "cfi",
        FrameTrust::PreWalked => "pre",
        FrameTrust::None => "?",
    };
    match &f.module {
        Some(m) => {
            let cf = m.code_file();
            let base = basename(&cf);
            let off = ip.wrapping_sub(m.raw.base_of_image);
            match (&f.function_name, &f.source_file_name, f.source_line) {
                (Some(func), Some(file), Some(line)) => {
                    format!("#{n:<3} {ip:#018x}  {base}`{func}  ({file}:{line})  [{trust}]")
                }
                (Some(func), _, _) => {
                    format!("#{n:<3} {ip:#018x}  {base}`{func}  [{trust}]")
                }
                _ => format!("#{n:<3} {ip:#018x}  {base} +{off:#x}  [{trust}]"),
            }
        }
        None => format!("#{n:<3} {ip:#018x}  ??? [{trust}]"),
    }
}

fn module_table<W: Write>(w: &mut W, modules: &MinidumpModuleList) -> std::io::Result<()> {
    if modules.iter().next().is_none() {
        return Ok(());
    }
    writeln!(w, "modules:")?;
    for m in modules.iter() {
        let id = m
            .debug_identifier()
            .map(|d| d.breakpad().to_string())
            .unwrap_or_else(|| "-".into());
        writeln!(
            w,
            "  {:#018x}  {:<40}  {}",
            m.raw.base_of_image,
            id,
            m.code_file()
        )?;
    }
    Ok(())
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}
