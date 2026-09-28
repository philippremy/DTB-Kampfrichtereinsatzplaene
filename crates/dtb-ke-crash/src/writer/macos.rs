//! macOS out-of-process capture: the Mach handshake (retrieve the carrier port the crashed process
//! installed on its task exception-port table before the crash — see `crate::macos::CARRIER_MASK` —
//! receive its task/thread port rights, `task_suspend` + `MinidumpWriter`).

#![allow(clippy::missing_safety_doc, unsafe_op_in_unsafe_fn)]

use std::io::Cursor;
use std::mem::{size_of, zeroed};

use crash_context::{CrashContext, ExceptionInfo};
use mach2::exception_types::{exception_behavior_t, exception_mask_t};
use mach2::kern_return::{KERN_SUCCESS, kern_return_t};
use mach2::mach_port::{mach_port_allocate, mach_port_insert_right};
use mach2::message::{
    MACH_MSG_SUCCESS, MACH_MSG_TIMEOUT_NONE, MACH_MSG_TYPE_COPY_SEND, MACH_MSG_TYPE_MAKE_SEND,
    MACH_MSGH_BITS, MACH_MSGH_BITS_COMPLEX, MACH_RCV_MSG, MACH_SEND_MSG, mach_msg, mach_msg_body_t,
    mach_msg_header_t, mach_msg_port_descriptor_t,
};
use mach2::port::{MACH_PORT_NULL, MACH_PORT_RIGHT_RECEIVE, mach_port_t};
use mach2::thread_status::thread_state_flavor_t;
use minidump_writer::minidump_writer::MinidumpWriter;

const MSG_ID_HELPER_HELLO: i32 = 0x6b63_0001;
const MSG_ID_FORWARD: i32 = 0x6b63_0002;
const MSG_ID_HELPER_DONE: i32 = 0x6b63_0003;

/// `mach/exception.h`'s `EXC_TYPES_COUNT` (incl. illegal exception 0) — not exposed by `mach2`.
const EXC_TYPES_COUNT: usize = 14;

unsafe extern "C" {
    fn pid_for_task(task: mach_port_t, pid: *mut i32) -> kern_return_t;
    fn task_suspend(task: mach_port_t) -> kern_return_t;
    fn task_resume(task: mach_port_t) -> kern_return_t;
    fn task_get_exception_ports(
        task: mach_port_t,
        exception_mask: exception_mask_t,
        masks: *mut exception_mask_t,
        masks_cnt: *mut u32,
        old_handlers: *mut mach_port_t,
        old_behaviors: *mut exception_behavior_t,
        old_flavors: *mut thread_state_flavor_t,
    ) -> kern_return_t;
}

/// Retrieves the carrier port the crashed process installed before it ever crashed (`fork`+`execv`
/// preserves task exception ports for free, no bootstrap namespace involved — see
/// `crate::macos::CARRIER_MASK`), receives its task/thread port rights over it, and reads its memory
/// via `task_suspend` + `MinidumpWriter` — pure aside from the Mach handshake itself, no spawn/exec
/// at all. Called directly by `dtb-ke-ui`'s self-relaunched capture path
/// (`crash_report::run_macos_capture`). Returns the dump bytes and the crashed process's pid.
pub unsafe fn capture() -> Option<(Vec<u8>, i32)> {
    // ── handshake ──
    // `S`'s send right is already sitting in our own (inherited via fork()+execv()) exception-port
    // table — a synchronous, local, sandbox-untouched retrieval, no lookup, no propagation delay, no
    // retry loop needed.
    let mut masks = [0 as exception_mask_t; EXC_TYPES_COUNT];
    let mut ports = [MACH_PORT_NULL; EXC_TYPES_COUNT];
    let mut behaviors = [0 as exception_behavior_t; EXC_TYPES_COUNT];
    let mut flavors = [0 as thread_state_flavor_t; EXC_TYPES_COUNT];
    let mut count: u32 = EXC_TYPES_COUNT as u32;
    let kr = task_get_exception_ports(
        mach2::traps::mach_task_self(),
        crate::macos::CARRIER_MASK,
        masks.as_mut_ptr(),
        &mut count,
        ports.as_mut_ptr(),
        behaviors.as_mut_ptr(),
        flavors.as_mut_ptr(),
    );
    if kr != KERN_SUCCESS || count == 0 {
        return None;
    }
    let s_send: mach_port_t = ports[0];
    if s_send == MACH_PORT_NULL {
        return None;
    }

    let me = mach2::traps::mach_task_self();
    let mut r: mach_port_t = MACH_PORT_NULL;
    if mach_port_allocate(me, MACH_PORT_RIGHT_RECEIVE, &mut r) != KERN_SUCCESS {
        return None;
    }
    mach_port_insert_right(me, r, r, MACH_MSG_TYPE_MAKE_SEND);

    // HELLO: send R to S.
    #[repr(C)]
    struct Hello {
        head: mach_msg_header_t,
        body: mach_msg_body_t,
        port: mach_msg_port_descriptor_t,
    }
    let mut hello: Hello = zeroed();
    hello.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0) | MACH_MSGH_BITS_COMPLEX;
    hello.head.msgh_size = size_of::<Hello>() as u32;
    hello.head.msgh_remote_port = s_send;
    hello.head.msgh_id = MSG_ID_HELPER_HELLO;
    hello.body.msgh_descriptor_count = 1;
    hello.port = mach_msg_port_descriptor_t::new(r, MACH_MSG_TYPE_MAKE_SEND);
    if mach_msg(
        &mut hello.head,
        MACH_SEND_MSG,
        size_of::<Hello>() as u32,
        0,
        MACH_PORT_NULL,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    ) != MACH_MSG_SUCCESS
    {
        return None;
    }

    // FORWARD: receive task + faulting thread + handler thread + exception
    // triple on R. Read back through the *exact same* `#[repr(C)]` struct the
    // sender lays out (`crate::macos`'s `handle_exception`'s `Forward`) — keep in sync.
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
    #[repr(C, align(8))]
    struct Buf([u8; 512]);
    let mut buf = Buf([0; 512]);
    if mach_msg(
        buf.0.as_mut_ptr() as *mut mach_msg_header_t,
        MACH_RCV_MSG,
        0,
        buf.0.len() as u32,
        r,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    ) != MACH_MSG_SUCCESS
    {
        return None;
    }
    if buf.0.len() < size_of::<Forward>() {
        return None;
    }
    let fwd = &*(buf.0.as_ptr() as *const Forward);
    if fwd.head.msgh_id != MSG_ID_FORWARD {
        return None;
    }
    // The kernel rewrites the descriptor `.name`s to our local port names.
    let task: mach_port_t = fwd.task.name;
    let thread: mach_port_t = fwd.thread.name;
    let handler_thread: mach_port_t = fwd.handler_thread.name;
    let exception = fwd.exception;
    let code = fwd.code;
    let subcode = fwd.subcode;

    let mut pid = 0i32;
    pid_for_task(task, &mut pid);

    // ── dump ── freeze the task for a consistent snapshot, then let
    // `minidump-writer` walk it via the forwarded ports.
    task_suspend(task);

    let cc = CrashContext {
        task,
        thread,
        handler_thread,
        exception: Some(ExceptionInfo { kind: exception, code, subcode: Some(subcode) }),
    };
    let mut cur = Cursor::new(Vec::<u8>::new());
    let dumped = MinidumpWriter::with_crash_context(cc).dump(&mut cur);

    task_resume(task);

    // ── done: the parent may exit now (it was blocked, hence suspended too;
    // the resume above lets it wake to receive this). ──
    #[repr(C)]
    struct Done {
        head: mach_msg_header_t,
        status: u32,
    }
    let mut done: Done = zeroed();
    done.head.msgh_bits = MACH_MSGH_BITS(MACH_MSG_TYPE_COPY_SEND, 0);
    done.head.msgh_size = size_of::<Done>() as u32;
    done.head.msgh_remote_port = s_send;
    done.head.msgh_id = MSG_ID_HELPER_DONE;
    let _ = mach_msg(
        &mut done.head,
        MACH_SEND_MSG,
        size_of::<Done>() as u32,
        0,
        MACH_PORT_NULL,
        MACH_MSG_TIMEOUT_NONE,
        MACH_PORT_NULL,
    );

    let bytes = dumped.ok()?;
    (!bytes.is_empty()).then_some((bytes, pid))
}
