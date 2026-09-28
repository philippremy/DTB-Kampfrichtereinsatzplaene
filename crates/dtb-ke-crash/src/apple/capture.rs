//! Crash-time capture: writes the `.crash` snapshot file (see `snapshot`) from inside a faulting
//! process using **only** syscalls / Mach traps, fixed buffers and raw `write` — no allocation, no
//! locks, no Rust `std` I/O, no third-party code. Every function it calls is pre-touched by
//! [`warm_up`] at install so no lazy-bind stub is first resolved on the crash path.

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn, static_mut_refs)]

use core::mem::{size_of, zeroed};
use libc::{c_int, c_void};
use mach2::kern_return::KERN_SUCCESS;
use mach2::mach_init::mach_thread_self;
use mach2::message::mach_msg_type_number_t;
use mach2::port::{MACH_PORT_NULL, mach_port_t};
use mach2::task::task_threads;
use mach2::thread_act::{thread_get_state, thread_resume, thread_suspend};
use mach2::traps::mach_task_self;
use mach2::vm::{mach_vm_read_overwrite, mach_vm_region};
use mach2::vm_region::{VM_REGION_BASIC_INFO_64, vm_region_basic_info_64};

use crate::snapshot::wire::*;
use crate::snapshot::{CRASH_MAGIC, FORMAT_VERSION};

unsafe extern "C" {
    fn thread_info(
        thread: mach_port_t,
        flavor: c_int,
        out: *mut c_int,
        count: *mut mach_msg_type_number_t,
    ) -> c_int;
}

const ARM_THREAD_STATE64: c_int = 6;
const THREAD_EXTENDED_INFO: c_int = 5;
/// `sizeof(thread_extended_info) / 4`.
const THREAD_EXTENDED_INFO_COUNT: u32 = 28;
/// The arm64 red zone: leaf functions keep data below `sp`.
const RED_ZONE: u64 = 128;
/// The crashing thread's stack slice cap; other threads get [`OTHER_STACK_CAP`].
const CRASH_STACK_CAP: usize = 256 * 1024;
const OTHER_STACK_CAP: usize = 64 * 1024;

#[repr(C)]
struct ArmThreadState64 {
    x: [u64; 29],
    fp: u64,
    lr: u64,
    sp: u64,
    pc: u64,
    cpsr: u32,
    flags: u32,
}

#[repr(C)]
struct ThreadExtendedInfo {
    user_time: u64,
    system_time: u64,
    ints: [i32; 8],
    name: [u8; THREAD_NAME_LEN],
}

/// The stack read buffer — BSS, so it costs nothing until a crash touches it.
static mut SCRATCH: [u8; CRASH_STACK_CAP] = [0; CRASH_STACK_CAP];

pub struct Params {
    /// Destination (already open, writable).
    pub fd: c_int,
    pub launch_id: [u8; 16],
    /// `(EXC_* kind, code, subcode)`; `None` for a snapshot with no fault (tests).
    pub exception: Option<(u32, u64, u64)>,
    /// The faulting thread's port name (`MACH_PORT_NULL` if none).
    pub crashing_thread: mach_port_t,
    /// The thread running this code — never suspended, never dumped.
    pub handler_thread: mach_port_t,
    /// NUL-terminated panic message, or null.
    pub panic_message: *const u8,
    /// Resume the threads we suspended when done (tests only; a real crash never resumes).
    pub resume: bool,
}

struct Sink {
    fd: c_int,
}

impl Sink {
    unsafe fn raw(&self, mut data: &[u8]) {
        while !data.is_empty() {
            let n = libc::write(self.fd, data.as_ptr() as *const c_void, data.len());
            if n <= 0 {
                return;
            }
            data = &data[n as usize..];
        }
    }

    /// `tag: u16, reserved: u16, len: u32` followed by the parts back to back.
    unsafe fn record(&self, tag: u16, parts: &[&[u8]]) {
        let len: usize = parts.iter().map(|p| p.len()).sum();
        let mut head = [0u8; 8];
        head[..2].copy_from_slice(&tag.to_le_bytes());
        head[4..].copy_from_slice(&(len as u32).to_le_bytes());
        self.raw(&head);
        for p in parts {
            self.raw(p);
        }
    }
}

/// Call every Mach trap / libc function [`capture`] uses once with harmless invalid arguments so any
/// lazily-bound stub is resolved now, while dyld's lock is uncontended.
pub unsafe fn warm_up() {
    let mut list: *mut mach_port_t = core::ptr::null_mut();
    let mut count: u32 = 0;
    let _ = task_threads(MACH_PORT_NULL, &mut list, &mut count);
    let mut st: ArmThreadState64 = zeroed();
    let mut n = (size_of::<ArmThreadState64>() / 4) as u32;
    let _ = thread_get_state(MACH_PORT_NULL, ARM_THREAD_STATE64, &mut st as *mut _ as *mut u32, &mut n);
    let mut ext: ThreadExtendedInfo = zeroed();
    let mut n = THREAD_EXTENDED_INFO_COUNT;
    let _ = thread_info(MACH_PORT_NULL, THREAD_EXTENDED_INFO, &mut ext as *mut _ as *mut c_int, &mut n);
    let _ = thread_suspend(MACH_PORT_NULL);
    let _ = thread_resume(MACH_PORT_NULL);
    let (mut addr, mut size, mut info, mut cnt, mut obj) = (0u64, 0u64, zeroed::<vm_region_basic_info_64>(), 9u32, 0u32);
    let _ = mach_vm_region(MACH_PORT_NULL, &mut addr, &mut size, VM_REGION_BASIC_INFO_64, &mut info as *mut _ as *mut c_int, &mut cnt, &mut obj);
    let mut out = 0u64;
    let _ = mach_vm_read_overwrite(MACH_PORT_NULL, 0, 0, 0, &mut out);
    let _ = libc::write(-1, b"".as_ptr() as *const c_void, 0);
    let _ = libc::getpid();
    let _ = libc::time(core::ptr::null_mut());
    let _ = mach_thread_self();
}

/// Write the crash record to `p.fd`. Order = importance: header, exception, panic message, the crashing
/// thread, the other threads, end marker.
pub unsafe fn capture(p: &Params) {
    let sink = Sink { fd: p.fd };
    let task = mach_task_self();

    sink.raw(CRASH_MAGIC);
    sink.raw(&FORMAT_VERSION.to_le_bytes());
    sink.raw(&0u32.to_le_bytes());

    let pid = libc::getpid() as u32;
    let now = libc::time(core::ptr::null_mut()) as u64;
    sink.record(
        TAG_HEADER,
        &[&p.launch_id, &pid.to_le_bytes(), &0u32.to_le_bytes(), &now.to_le_bytes()],
    );

    if let Some((kind, code, subcode)) = p.exception {
        sink.record(
            TAG_EXCEPTION,
            &[
                &kind.to_le_bytes(),
                &0u32.to_le_bytes(),
                &code.to_le_bytes(),
                &subcode.to_le_bytes(),
                &(p.crashing_thread as u64).to_le_bytes(),
            ],
        );
    }

    if !p.panic_message.is_null() && *p.panic_message != 0 {
        let mut len = 0usize;
        while len < 4096 && *p.panic_message.add(len) != 0 {
            len += 1;
        }
        sink.record(TAG_PANIC, &[core::slice::from_raw_parts(p.panic_message, len)]);
    }

    let mut list: *mut mach_port_t = core::ptr::null_mut();
    let mut count: u32 = 0;
    if task_threads(task, &mut list, &mut count) == KERN_SUCCESS && !list.is_null() {
        let threads = core::slice::from_raw_parts(list, count as usize);
        for &t in threads {
            if t != p.handler_thread {
                thread_suspend(t);
            }
        }
        // The crashing thread first, then the rest.
        if p.crashing_thread != MACH_PORT_NULL {
            dump_thread(&sink, task, p.crashing_thread, true);
        }
        for &t in threads {
            if t != p.handler_thread && t != p.crashing_thread {
                dump_thread(&sink, task, t, false);
            }
        }
        if p.resume {
            for &t in threads {
                if t != p.handler_thread {
                    thread_resume(t);
                }
            }
        }
    }

    sink.record(TAG_END, &[]);
}

unsafe fn dump_thread(sink: &Sink, task: mach_port_t, thread: mach_port_t, crashed: bool) {
    let mut st: ArmThreadState64 = zeroed();
    let mut n = (size_of::<ArmThreadState64>() / 4) as u32;
    if thread_get_state(thread, ARM_THREAD_STATE64, &mut st as *mut _ as *mut u32, &mut n) != KERN_SUCCESS {
        return;
    }
    let mut ext: ThreadExtendedInfo = zeroed();
    let mut m = THREAD_EXTENDED_INFO_COUNT;
    if thread_info(thread, THREAD_EXTENDED_INFO, &mut ext as *mut _ as *mut c_int, &mut m) != KERN_SUCCESS {
        ext.name = [0; THREAD_NAME_LEN];
    }

    let mut regs = [0u64; ARM64_REGS];
    regs[..29].copy_from_slice(&st.x);
    regs[29] = st.fp;
    regs[30] = st.lr;
    regs[31] = st.sp;
    regs[32] = st.pc;
    regs[33] = st.cpsr as u64;
    let mut regs_bytes = [0u8; ARM64_REGS * 8];
    for (i, r) in regs.iter().enumerate() {
        regs_bytes[i * 8..i * 8 + 8].copy_from_slice(&r.to_le_bytes());
    }
    let flags = if crashed { THREAD_FLAG_CRASHED } else { 0 };
    sink.record(
        TAG_THREAD,
        &[&(thread as u64).to_le_bytes(), &flags.to_le_bytes(), &0u32.to_le_bytes(), &regs_bytes, &ext.name],
    );

    let cap = if crashed { CRASH_STACK_CAP } else { OTHER_STACK_CAP };
    if let Some((addr, len)) = read_stack(task, st.sp, st.fp, cap) {
        sink.record(
            TAG_STACK,
            &[&(thread as u64).to_le_bytes(), &addr.to_le_bytes(), &SCRATCH[..len]],
        );
    }
}

/// Copy a slice of the stack into [`SCRATCH`]: from just below `sp` to the end of its mapping, capped.
/// A stack overflow leaves `sp` in an unmapped guard page — then fall back to the frame pointer, and
/// failing that to the start of the next mapping (the deepest frames that still exist).
unsafe fn read_stack(task: mach_port_t, sp: u64, fp: u64, cap: usize) -> Option<(u64, usize)> {
    for anchor in [sp, fp] {
        if anchor == 0 {
            continue;
        }
        let start = anchor.saturating_sub(RED_ZONE);
        if let Some(end) = region_end(task, start) {
            let mut len = (end - start).min(cap as u64) as usize;
            while len >= 256 {
                let mut out = 0u64;
                if mach_vm_read_overwrite(task, start, len as u64, SCRATCH.as_mut_ptr() as u64, &mut out)
                    == KERN_SUCCESS
                {
                    return Some((start, out as usize));
                }
                len /= 2;
            }
        }
    }
    None
}

/// End of the mapping that contains `addr`, or `None` if `addr` is not mapped.
unsafe fn region_end(task: mach_port_t, addr: u64) -> Option<u64> {
    let (mut a, mut size, mut cnt, mut obj) = (addr, 0u64, 9u32, 0u32);
    let mut info: vm_region_basic_info_64 = zeroed();
    let kr = mach_vm_region(task, &mut a, &mut size, VM_REGION_BASIC_INFO_64, &mut info as *mut _ as *mut c_int, &mut cnt, &mut obj);
    if kr != KERN_SUCCESS || a > addr {
        return None;
    }
    Some(a + size)
}
