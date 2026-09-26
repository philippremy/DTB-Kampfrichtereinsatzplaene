//! Differential test of the signal-safe crash-record writer (arm64 macOS): capture *this* process while
//! a helper thread is parked, assemble a minidump from the snapshot, and compare it with what
//! `minidump-writer` (the desktop helper's engine) reports for the same task.
#![cfg(all(target_os = "macos", target_arch = "aarch64"))]

use std::io::Cursor;
use std::os::fd::AsRawFd;
use std::sync::mpsc;

use dtb_ke_crash::apple::{capture, session};
use dtb_ke_crash::snapshot::{self, ExceptionDTO};
use minidump::{Minidump, MinidumpModuleList, MinidumpThreadList, Module};

unsafe extern "C" {
    fn mach_thread_self() -> u32;
}

/// Park a named thread; returns its mach port name and a sender that releases it.
fn parked_thread() -> (u32, mpsc::Sender<()>, std::thread::JoinHandle<()>) {
    let (port_tx, port_rx) = mpsc::channel();
    let (go_tx, go_rx) = mpsc::channel::<()>();
    let handle = std::thread::Builder::new()
        .name("capture-target".into())
        .spawn(move || {
            port_tx.send(unsafe { mach_thread_self() }).unwrap();
            let _ = go_rx.recv();
        })
        .unwrap();
    (port_rx.recv().unwrap(), go_tx, handle)
}

#[test]
fn capture_matches_minidump_writer() {
    unsafe { capture::warm_up() };
    let (target, release, join) = parked_thread();
    std::thread::sleep(std::time::Duration::from_millis(100)); // let it reach recv()

    let launch_id = [0x5a; 16];
    let sess = session::current(launch_id, "test");
    let file = tempfile_path("capture.crash");
    let f = std::fs::File::create(&file).unwrap();
    unsafe {
        capture::capture(&capture::Params {
            fd: f.as_raw_fd(),
            launch_id,
            exception: Some((1, 1, 0x10)),
            crashing_thread: target,
            handler_thread: mach_thread_self(),
            panic_message: b"panicked at test\0".as_ptr(),
            resume: true,
        });
    }
    drop(f);

    let crash = std::fs::read(&file).unwrap();
    let snap = snapshot::parse(&crash, Some(&snapshot::session::encode(&sess))).unwrap();
    assert!(snap.complete);
    assert_eq!(snap.exception, Some(ExceptionDTO { kind: 1, code: 1, subcode: 0x10, thread_id: target as u64 }));
    assert_eq!(snap.panic_message.as_deref(), Some("panicked at test"));

    let crashed = snap.threads.iter().find(|t| t.crashed).expect("crashing thread recorded first");
    assert_eq!(crashed.id, target as u64);
    assert_eq!(crashed.name, "capture-target");
    assert_ne!(crashed.regs.pc, 0);
    let stack = crashed.stack.as_ref().expect("stack captured");
    assert!(stack.address <= crashed.regs.sp && crashed.regs.sp < stack.address + stack.data.len() as u64);
    // The parked thread sits in a real call chain, so the fp walk finds more than pc.
    assert!(snapshot::summary::walk(crashed).len() >= 3, "{:x?}", snapshot::summary::walk(crashed));

    // ── minidump-writer's view of the same task ──
    let mut mdw = minidump_writer::minidump_writer::MinidumpWriter::new(None, None);
    let mut cur = Cursor::new(Vec::new());
    let theirs = mdw.dump(&mut cur).expect("minidump-writer dump");
    let theirs = Minidump::read(theirs).unwrap();
    let their_modules = theirs.get_stream::<MinidumpModuleList>().unwrap();
    let their_main = their_modules.main_module().unwrap();

    let ours = Minidump::read(snapshot::to_minidump(&snap)).unwrap();
    let our_modules = ours.get_stream::<MinidumpModuleList>().unwrap();
    let our_main = our_modules.main_module().unwrap();
    assert_eq!(our_main.base_address(), their_main.base_address());
    assert_eq!(our_main.size(), their_main.size());
    assert_eq!(our_main.debug_identifier(), their_main.debug_identifier());

    let their_threads = theirs.get_stream::<MinidumpThreadList>().unwrap();
    let their_target = their_threads.get_thread(target).expect("target thread in their dump");
    let our_threads = ours.get_stream::<MinidumpThreadList>().unwrap();
    let our_target = our_threads.get_thread(target).expect("target thread in our dump");
    // minidump-writer starts at sp; we start one arm64 red zone (128 bytes) lower, where leaf frames live.
    assert_eq!(
        our_target.raw.stack.start_of_memory_range + 128,
        their_target.raw.stack.start_of_memory_range,
    );

    release.send(()).unwrap();
    join.join().unwrap();
    let _ = std::fs::remove_file(file);
}

fn tempfile_path(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("dtbke-{}-{name}", std::process::id()))
}
