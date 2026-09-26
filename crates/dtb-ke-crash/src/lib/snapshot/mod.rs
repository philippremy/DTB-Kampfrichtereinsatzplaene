//! The crash **snapshot**: a crash-time wire format, its parser, and the assembler that turns it into a
//! standard minidump at the next launch.
//!
//! Why: on iOS there is no helper process, so the faulting process itself must record what it can
//! with *no allocation and no third-party code* (see `apple/`), and a later, healthy launch converts
//! that into a `.dmp` — the same artifact the desktop helper produces, so reports look identical.
//! Everything here is plain Rust with no OS calls, so it is unit-tested on any host.
//!
//! Two little-endian, tag-length-value files per launch, both starting with an 8-byte magic + `u32`
//! version + `u32` reserved:
//!
//! * **session** (`<slug>-<launch>.session`, written at launch by normal allocating code, rewritten
//!   when images load): OS / app facts and the module list — everything that does not need to be read
//!   at crash time;
//! * **crash** (`<slug>-<launch>.crash`, written by the fault handler): the exception, the panic
//!   message, every thread's registers and a bounded slice of its stack. Records are appended most
//!   important first, so a file cut short by the watchdog is still usable ([`CrashSnapshotDTO::complete`]).
//!
//! A record is `tag: u16, reserved: u16, len: u32, payload[len]`.

mod assemble;
mod parse;
pub mod pending;
pub mod session;
pub mod summary;

pub use assemble::to_minidump;
pub use parse::{SnapshotError, parse};

pub const CRASH_MAGIC: &[u8; 8] = b"DTBKECR1";
pub const SESSION_MAGIC: &[u8; 8] = b"DTBKESS1";
pub const FORMAT_VERSION: u32 = 1;

/// Wire constants shared with the crash-time writer.
pub mod wire {
    pub const TAG_HEADER: u16 = 1;
    pub const TAG_EXCEPTION: u16 = 2;
    pub const TAG_PANIC: u16 = 3;
    pub const TAG_THREAD: u16 = 4;
    pub const TAG_STACK: u16 = 5;
    pub const TAG_END: u16 = 0xFFFF;

    pub const TAG_SESSION_HEADER: u16 = 0x100;
    pub const TAG_KV: u16 = 0x101;
    pub const TAG_MODULE: u16 = 0x102;

    /// x0..x28, fp, lr, sp, pc, cpsr.
    pub const ARM64_REGS: usize = 34;
    pub const THREAD_NAME_LEN: usize = 64;
    /// `tid u64, flags u32, pad u32, regs, name`.
    pub const THREAD_PAYLOAD: usize = 8 + 4 + 4 + ARM64_REGS * 8 + THREAD_NAME_LEN;
    pub const THREAD_FLAG_CRASHED: u32 = 1;
    pub const HEADER_PAYLOAD: usize = 16 + 4 + 4 + 8;
    pub const EXCEPTION_PAYLOAD: usize = 4 + 4 + 8 + 8 + 8;
    pub const MODULE_FIXED: usize = 8 + 8 + 16 + 4 + 4;
    pub const MODULE_FLAG_MAIN: u32 = 1;
}

/// Everything the crash handler and the launch-time snapshotter recorded about one crashed run.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CrashSnapshotDTO {
    pub launch_id: [u8; 16],
    pub pid: u32,
    pub crash_time_unix: u64,
    pub exception: Option<ExceptionDTO>,
    pub panic_message: Option<String>,
    pub threads: Vec<ThreadDTO>,
    pub session: SessionDTO,
    /// The crash file ended with its end marker (the handler finished).
    pub complete: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ExceptionDTO {
    /// `EXC_*`.
    pub kind: u32,
    pub code: u64,
    pub subcode: u64,
    pub thread_id: u64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ThreadDTO {
    pub id: u64,
    pub crashed: bool,
    pub name: String,
    pub regs: Arm64RegsDTO,
    pub stack: Option<StackDTO>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Arm64RegsDTO {
    pub x: [u64; 29],
    pub fp: u64,
    pub lr: u64,
    pub sp: u64,
    pub pc: u64,
    pub cpsr: u32,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct StackDTO {
    pub address: u64,
    pub data: Vec<u8>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SessionDTO {
    pub launch_id: [u8; 16],
    pub pid: u32,
    pub app_version: String,
    pub os_version: String,
    pub os_build: String,
    pub machine: String,
    pub ncpu: u32,
    pub exe_path: String,
    /// The build-info payload (`buildinfo` format), empty if the app supplied none.
    pub build_info: String,
    pub modules: Vec<ModuleDTO>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ModuleDTO {
    pub base: u64,
    pub size: u64,
    pub uuid: [u8; 16],
    pub is_main: bool,
    pub path: String,
}
