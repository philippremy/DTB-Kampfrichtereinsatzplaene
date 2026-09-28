//! The snapshot format and its minidump assembler, exercised with a synthetic crash: a frame-pointer
//! chain over a hand-built stack, read back through the real `minidump` reader and unwound with
//! `minidump-unwind` — the same stack `dtb-ke-debugger` uses.

use dtb_ke_crash::snapshot::{self, *};
use minidump::{
    Minidump, MinidumpException, MinidumpModuleList, MinidumpSystemInfo,
    MinidumpThreadList, MinidumpThreadNames, Module,
};
use minidump_unwind::{CallStack, SystemInfo, walk_stack};
use minidump_unwind::symbols::MultiSymbolProvider;

const STACK_BASE: u64 = 0x1_6000_0000;
const TEXT: u64 = 0x1_0000_0000;

/// A stack whose frame records chain `fp0 → fp1 → fp2 → 0`, returning into three distinct places.
fn synthetic() -> CrashSnapshotDTO {
    let mut stack = vec![0u8; 0x400];
    let mut put = |addr: u64, v: u64| {
        let off = (addr - STACK_BASE) as usize;
        stack[off..off + 8].copy_from_slice(&v.to_le_bytes());
    };
    let (fp0, fp1, fp2) = (STACK_BASE + 0x100, STACK_BASE + 0x180, STACK_BASE + 0x200);
    put(fp0, fp1);
    put(fp0 + 8, TEXT + 0x2200);
    put(fp1, fp2);
    put(fp1 + 8, TEXT + 0x3300);
    put(fp2, 0);
    put(fp2 + 8, 0);

    let mut regs = Arm64RegsDTO::default();
    regs.pc = TEXT + 0x1100;
    regs.lr = TEXT + 0x2200;
    regs.fp = fp0;
    regs.sp = STACK_BASE + 0xf0;
    regs.x[0] = 0xdead_beef;

    let session = SessionDTO {
        launch_id: [7; 16],
        pid: 4242,
        app_version: "1.2.3".into(),
        os_version: "26.0.1".into(),
        os_build: "23A123".into(),
        machine: "iPad14,1".into(),
        ncpu: 8,
        exe_path: "/var/containers/Bundle/Application/X/App.app/App".into(),
        build_info: "app_version=1.2.3\ncommit=abc123\ntarget=aarch64-apple-ios\n".into(),
        modules: vec![
            ModuleDTO { base: 0x1_8000_0000, size: 0x10_0000, uuid: [9; 16], is_main: false, path: "/usr/lib/system/libsystem_c.dylib".into() },
            ModuleDTO { base: TEXT, size: 0x8000, uuid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16], is_main: true, path: "/var/containers/Bundle/Application/X/App.app/App".into() },
        ],
    };

    CrashSnapshotDTO {
        launch_id: [7; 16],
        pid: 4242,
        crash_time_unix: 1_800_000_000,
        exception: Some(ExceptionDTO { kind: 1, code: 1, subcode: 0x10, thread_id: 0xabc }),
        panic_message: Some("panicked at src/main.rs:1:1:\nboom".into()),
        threads: vec![
            ThreadDTO { id: 0x111, crashed: false, name: "worker".into(), regs: Arm64RegsDTO::default(), stack: None },
            ThreadDTO { id: 0xabc, crashed: true, name: "main".into(), regs, stack: Some(StackDTO { address: STACK_BASE, data: stack }) },
        ],
        session,
        complete: true,
    }
}

#[test]
fn wire_round_trip() {
    let snap = synthetic();
    let crash = session::encode_crash(&snap);
    let sess = session::encode(&snap.session);
    let parsed = snapshot::parse(&crash, Some(&sess)).unwrap();
    assert_eq!(parsed, snap);
}

#[test]
fn truncated_crash_file_keeps_what_was_written() {
    let snap = synthetic();
    let mut crash = session::encode_crash(&snap);
    crash.truncate(crash.len() - 200); // cut inside the last stack record
    let parsed = snapshot::parse(&crash, None).unwrap();
    assert!(!parsed.complete);
    assert_eq!(parsed.threads.len(), 2);
    assert_eq!(parsed.exception, snap.exception);
}

#[test]
fn rejects_foreign_files() {
    assert!(matches!(snapshot::parse(b"not a crash file at all", None), Err(SnapshotError::BadMagic)));
}

#[test]
fn minidump_reads_back_and_unwinds() {
    let snap = synthetic();
    let dump = Minidump::read(snapshot::to_minidump(&snap)).expect("valid minidump");

    let sys = dump.get_stream::<MinidumpSystemInfo>().unwrap();
    assert_eq!(format!("{:?}", sys.os), "Ios");
    assert_eq!(format!("{:?}", sys.cpu), "Arm64");

    let modules = dump.get_stream::<MinidumpModuleList>().unwrap();
    let main = modules.main_module().unwrap();
    assert_eq!(main.base_address(), TEXT);
    assert_eq!(main.debug_identifier().unwrap().to_string(), "01020304-0506-0708-090a-0b0c0d0e0f10");

    let exc = dump.get_stream::<MinidumpException>().unwrap();
    assert_eq!(exc.get_crashing_thread_id(), 0xabc);

    let names = dump.get_stream::<MinidumpThreadNames>().unwrap();
    assert_eq!(names.get_name(0xabc).unwrap(), "main");

    let threads = dump.get_stream::<MinidumpThreadList>().unwrap();
    let memory = dump.get_memory().unwrap_or_default();
    let thread = threads.get_thread(0xabc).unwrap();
    let context = thread.context(&sys, None).expect("arm64 context");

    let unwind_sys = SystemInfo {
        os: sys.os,
        cpu: sys.cpu,
        os_version: None,
        os_build: None,
        cpu_info: None,
        cpu_microcode_version: None,
        cpu_count: 1,
    };
    let mut stack = CallStack::with_context(context.into_owned());
    futures::executor::block_on(walk_stack(
        0,
        (),
        &mut stack,
        thread.stack_memory(&memory),
        &modules,
        &unwind_sys,
        &MultiSymbolProvider::new(),
    ));
    let pcs: Vec<u64> = stack.frames.iter().map(|f| f.resume_address).collect();
    assert_eq!(pcs[0], TEXT + 0x1100);
    assert!(pcs.contains(&(TEXT + 0x2200)), "frames: {pcs:x?}");
    assert!(pcs.contains(&(TEXT + 0x3300)), "frames: {pcs:x?}");
}

#[test]
fn digest_names_frames_by_module_offset() {
    let d = snapshot::summary::digest(&synthetic());
    assert!(d.reason.starts_with("EXC_BAD_ACCESS"));
    assert_eq!(d.address, 0x10);
    assert_eq!(d.crashing_thread, 0xabc);
    assert!(d.panic_message.contains("boom"));
    assert!(d.stack.contains("#0 App +0x1100"), "{}", d.stack);
    assert!(d.stack.contains("#1 App +0x2200"), "{}", d.stack);
    assert!(d.stack.contains("#2 App +0x3300"), "{}", d.stack);
}

/// A synthetic `EXC_SOFTWARE`/`EXC_SOFT_SIGNAL` exception (the shape `macos`/`ios` build for the
/// `FATAL_SIGNALS` — see their module doc comments) names the real signal in the digest's reason,
/// not just the bare Mach exception kind.
#[test]
fn digest_names_the_real_signal_behind_a_synthetic_exc_software() {
    let mut snap = synthetic();
    snap.exception = Some(ExceptionDTO { kind: 5, code: 0x10003, subcode: 6, thread_id: 0xabc });
    let d = snapshot::summary::digest(&snap);
    assert!(d.reason.starts_with("EXC_SOFTWARE / SIGABRT"), "{}", d.reason);

    // An unrecognized subcode (not one of `FATAL_SIGNALS`) falls back to the bare exception name.
    snap.exception = Some(ExceptionDTO { kind: 5, code: 0x10003, subcode: 999, thread_id: 0xabc });
    let d = snapshot::summary::digest(&snap);
    assert!(d.reason.starts_with("EXC_SOFTWARE ("), "{}", d.reason);

    // A hardware fault (not EXC_SOFTWARE at all) is untouched.
    let d = snapshot::summary::digest(&synthetic());
    assert!(!d.reason.contains('/'), "{}", d.reason);
}

const INFO: &str = "app_version=1.2.3\ncommit=abc123\ntarget=aarch64-apple-ios\n";

fn user_stream(dmp: Vec<u8>) -> Option<String> {
    let dump = Minidump::read(dmp).expect("valid minidump");
    dump.get_raw_stream(dtb_ke_crash::buildinfo::STREAM_TYPE)
        .ok()
        .map(|b| String::from_utf8(b.to_vec()).unwrap())
}

#[test]
fn assembled_dump_carries_the_build_info_stream() {
    let text = user_stream(snapshot::to_minidump(&synthetic())).expect("stream present");
    assert_eq!(text, INFO);
    let parsed = dtb_ke_crash::buildinfo::parse(&text);
    assert_eq!(parsed["commit"], "abc123");
    assert_eq!(parsed["target"], "aarch64-apple-ios");
}

#[test]
fn patching_appends_a_stream_and_keeps_the_rest_readable() {
    use dtb_ke_crash::{buildinfo::STREAM_TYPE, patch};
    let mut snap = synthetic();
    snap.session.build_info.clear();
    let mut dmp = snapshot::to_minidump(&snap);
    assert!(user_stream(dmp.clone()).is_none());
    let streams_before = Minidump::read(dmp.clone()).unwrap().all_streams().count();

    patch::append_stream(&mut dmp, STREAM_TYPE, INFO.as_bytes()).unwrap();
    assert_eq!(dmp.len() % 4, 0, "directory stays 4-byte aligned");
    let dump = Minidump::read(dmp.clone()).unwrap();
    assert_eq!(dump.all_streams().count(), streams_before + 1);
    assert!(dump.get_stream::<MinidumpException>().is_ok(), "existing streams unharmed");
    assert!(dump.get_stream::<MinidumpModuleList>().is_ok());
    assert_eq!(user_stream(dmp.clone()).unwrap(), INFO);

    // Patching again replaces rather than duplicating.
    patch::append_stream(&mut dmp, STREAM_TYPE, b"commit=new\n").unwrap();
    let dump = Minidump::read(dmp.clone()).unwrap();
    assert_eq!(dump.all_streams().count(), streams_before + 1);
    assert_eq!(user_stream(dmp).unwrap(), "commit=new\n");
}

#[test]
fn patching_rejects_garbage_without_touching_it() {
    use dtb_ke_crash::patch::{self, PatchError};
    let mut junk = b"definitely not a minidump, but long enough to have a header".to_vec();
    let before = junk.clone();
    assert_eq!(patch::append_stream(&mut junk, 1, b"x"), Err(PatchError::NotAMinidump));
    assert_eq!(junk, before);

    let mut dmp = snapshot::to_minidump(&synthetic());
    dmp[12..16].copy_from_slice(&0xffff_fff0u32.to_le_bytes()); // directory RVA past the end
    let before = dmp.clone();
    assert_eq!(patch::append_stream(&mut dmp, 1, b"x"), Err(PatchError::Corrupt));
    assert_eq!(dmp, before);
}

#[test]
fn build_info_render_round_trips_and_flattens_newlines() {
    use dtb_ke_crash::buildinfo::{parse, render};
    let text = render([("a", "1".to_string()), ("linker", "clang\n21.0=x".to_string())]);
    let p = parse(&text);
    assert_eq!(p["a"], "1");
    assert_eq!(p["linker"], "clang 21.0=x");
}
