//! The reporter's digest, derived straight from a snapshot (no minidump reader needed).

use super::*;

/// What the crash reporter shows / mails, the same fields the desktop helper pipes to it.
#[derive(Clone, Debug, PartialEq)]
pub struct Digest {
    pub reason: String,
    pub address: u64,
    pub crashing_thread: u64,
    pub panic_message: String,
    /// One frame per line, symbol-free: `#0 <module> +0x<offset>` (resolved offline against the dSYM).
    pub stack: String,
}

fn exception_name(kind: u32) -> &'static str {
    match kind {
        1 => "EXC_BAD_ACCESS",
        2 => "EXC_BAD_INSTRUCTION",
        3 => "EXC_ARITHMETIC",
        4 => "EXC_EMULATION",
        5 => "EXC_SOFTWARE",
        6 => "EXC_BREAKPOINT",
        10 => "EXC_CRASH",
        11 => "EXC_RESOURCE",
        12 => "EXC_GUARD",
        _ => "EXC_UNKNOWN",
    }
}

pub fn module_for(session: &SessionDTO, addr: u64) -> Option<&ModuleDTO> {
    session.modules.iter().find(|m| addr >= m.base && addr < m.base + m.size)
}

/// Frame-pointer walk over the thread's captured stack: `pc`, then the saved return addresses of the
/// `(fp, lr)` frame records the stack slice contains. The `lr` register is added too, unless it is
/// the first record's return address (a frameless leaf function returns through `lr` alone, so its
/// caller would otherwise be missing).
pub fn walk(thread: &ThreadDTO) -> Vec<u64> {
    let mut frames = vec![thread.regs.pc];
    let Some(stack) = &thread.stack else {
        if thread.regs.lr != 0 {
            frames.push(thread.regs.lr);
        }
        return frames;
    };
    let read = |addr: u64| -> Option<u64> {
        let off = addr.checked_sub(stack.address)? as usize;
        Some(u64::from_le_bytes(stack.data.get(off..off + 8)?.try_into().ok()?))
    };
    let mut chain = Vec::new();
    let mut fp = thread.regs.fp;
    while chain.len() < 63 && fp != 0 && fp % 8 == 0 {
        let (Some(next), Some(ret)) = (read(fp), read(fp + 8)) else { break };
        if ret == 0 {
            break;
        }
        chain.push(ret);
        if next <= fp {
            break;
        }
        fp = next;
    }
    if thread.regs.lr != 0 && chain.first() != Some(&thread.regs.lr) {
        frames.push(thread.regs.lr);
    }
    frames.extend(chain);
    frames
}

pub fn digest(snap: &CrashSnapshotDTO) -> Digest {
    let crashed = snap
        .threads
        .iter()
        .find(|t| t.crashed)
        .or_else(|| snap.exception.as_ref().and_then(|e| snap.threads.iter().find(|t| t.id == e.thread_id)));

    let (reason, address) = match &snap.exception {
        Some(e) => (
            format!("{} (code {:#x}, subcode {:#x})", exception_name(e.kind), e.code, e.subcode),
            if e.kind == 1 { e.subcode } else { crashed.map(|t| t.regs.pc).unwrap_or(0) },
        ),
        None => ("no exception recorded".to_string(), 0),
    };

    let mut stack = String::new();
    if let Some(t) = crashed {
        for (i, pc) in walk(t).into_iter().enumerate() {
            // Return addresses point after the call; look up the call instruction's module.
            let lookup = if i == 0 { pc } else { pc.saturating_sub(1) };
            match module_for(&snap.session, lookup) {
                Some(m) => {
                    let name = m.path.rsplit('/').next().unwrap_or(&m.path);
                    stack.push_str(&format!("#{i} {name} +{:#x}\n", pc - m.base));
                }
                None => stack.push_str(&format!("#{i} ?? {pc:#x}\n")),
            }
        }
    }

    Digest {
        reason,
        address,
        crashing_thread: crashed.map(|t| t.id).unwrap_or(0),
        panic_message: snap.panic_message.clone().unwrap_or_default(),
        stack,
    }
}
