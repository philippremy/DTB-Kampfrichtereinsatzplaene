//! Raw-stream listing / rendering and the build-info reader, on a synthetic (iOS-assembler) dump.

use dtb_ke_crash::snapshot::{
    CrashSnapshotDTO, ExceptionDTO, ModuleDTO, SessionDTO, ThreadDTO, to_minidump,
};
use dtb_ke_debugger::{BuildInfo, rawdump};
use minidump::Minidump;

fn dump() -> Minidump<'static, Vec<u8>> {
    let snap = CrashSnapshotDTO {
        exception: Some(ExceptionDTO {
            kind: 1,
            code: 1,
            subcode: 0x10,
            thread_id: 5,
        }),
        threads: vec![ThreadDTO {
            id: 5,
            crashed: true,
            name: "main".into(),
            ..Default::default()
        }],
        session: SessionDTO {
            build_info: "app_version=1.2.3\ncommit_full=abc\nworkspace_root=/ci/repo\n".into(),
            modules: vec![ModuleDTO {
                base: 0x1000,
                size: 0x100,
                uuid: [3; 16],
                is_main: true,
                path: "/x/App".into(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    Minidump::read(to_minidump(&snap)).expect("valid dump")
}

#[test]
fn build_info_is_read_from_the_user_stream() {
    let b = BuildInfo::from_dump(&dump()).expect("stream");
    assert_eq!(b.app_version(), Some("1.2.3"));
    assert_eq!(b.commit(), Some("abc"));
    assert_eq!(b.workspace_root(), Some("/ci/repo"));
    assert!(!b.dirty());
}

#[test]
fn streams_are_listed_and_rendered() {
    let d = dump();
    let list = rawdump::streams(&d);
    let names: Vec<&str> = list.iter().map(|s| s.name.as_str()).collect();
    assert!(
        names.contains(&"ModuleListStream") && names.contains(&"BuildInfoStream"),
        "{names:?}"
    );
    assert!(list.iter().all(|s| s.understood));

    let build = list.iter().find(|s| s.name == "BuildInfoStream").unwrap();
    assert_eq!(build.vendor, "DTB KE");
    assert!(rawdump::stream_text(&d, build.type_id).contains("commit_full=abc"));

    let modules = list.iter().find(|s| s.name == "ModuleListStream").unwrap();
    assert!(rawdump::stream_text(&d, modules.type_id).contains("/x/App"));
}

#[test]
fn nsexception_stream_is_appended_and_read_back() {
    // Unlike build-info/syshints, the iOS-assembler `dump()` fixture never carries this stream (it's
    // macOS-only, appended by the crash helper via `patch::append_stream`) — so append it the same way.
    use dtb_ke_crash::nsexception::{self, NsException};
    let ex = NsException {
        name: "NSInternalInconsistencyException".into(),
        reason: "An instance was deallocated while key value observers were still registered."
            .into(),
        frames: vec!["0   AppKit   0x1 -[NSApplication _crashOnException:] + 1".into()],
    };
    let mut bytes = dtb_ke_crash::snapshot::to_minidump(&CrashSnapshotDTO::default());
    dtb_ke_crash::patch::append_stream(&mut bytes, nsexception::STREAM_TYPE, nsexception::render(&ex).as_bytes())
        .unwrap();
    let d = Minidump::read(bytes).expect("valid dump");

    let raw = d.get_raw_stream(nsexception::STREAM_TYPE).expect("stream present");
    assert_eq!(nsexception::parse(&String::from_utf8_lossy(raw)), Some(ex));

    let list = rawdump::streams(&d);
    let entry = list.iter().find(|s| s.name == "NSExceptionStream").expect("named");
    assert_eq!(entry.vendor, "DTB KE");
    assert!(entry.understood);
    assert!(rawdump::stream_text(&d, entry.type_id).contains("NSInternalInconsistencyException"));
}

#[test]
fn a_relative_main_module_path_is_replaced_by_the_build_infos_absolute_exe_path() {
    let snap = CrashSnapshotDTO {
        session: SessionDTO {
            build_info: "exe_path=/abs/target/debug/app\n".into(),
            modules: vec![ModuleDTO {
                base: 0x1000,
                size: 0x100,
                uuid: [3; 16],
                is_main: true,
                path: "target/debug/app".into(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let path = std::env::temp_dir().join(format!("dtbke-rel-{}.dtbkedmp", std::process::id()));
    std::fs::write(&path, to_minidump(&snap)).unwrap();
    let opened = dtb_ke_debugger::process::OpenedDump::open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(opened.modules[0].code_file, "/abs/target/debug/app");
}

/// A dump whose module list has `libx.dylib` (uuid 0x42…) and which carries a system-symbol stream.
fn dump_with_system_hints(stream: &[u8]) -> Minidump<'static, Vec<u8>> {
    let snap = CrashSnapshotDTO {
        session: SessionDTO {
            modules: vec![ModuleDTO {
                base: 0x1_8000_0000,
                size: 0x10_0000,
                uuid: [0x42; 16],
                is_main: false,
                path: "/usr/lib/libx.dylib".into(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bytes = to_minidump(&snap);
    dtb_ke_crash::patch::append_stream(&mut bytes, dtb_ke_crash::syshints::STREAM_TYPE, stream)
        .unwrap();
    Minidump::read(bytes).expect("valid dump")
}

#[test]
fn the_system_symbol_stream_has_a_name_and_a_structured_view() {
    use dtb_ke_crash::syshints::{Entry, Hints, Quality};
    let hints = Hints {
        producer: "macos-dyld-cache".into(),
        quality: Quality::Exact,
        complete: false,
        incomplete_reason: "skipped".into(),
        frames_total: 93,
        frames_resolved: 71,
        os_build: "26A428".into(),
        entries: vec![
            Entry {
                uuid: [0x42; 16],
                start: 0x2000,
                end: 0x2040,
                name: "_second".into(),
            },
            Entry {
                uuid: [0x42; 16],
                start: 0x1000,
                end: 0x1080,
                name: "_first".into(),
            },
            Entry {
                uuid: [0x99; 16],
                start: 0x10,
                end: 0x20,
                name: "_orphan".into(),
            },
        ],
    };
    let d = dump_with_system_hints(hints.encode().as_bytes());

    let list = rawdump::streams(&d);
    let stream = list
        .iter()
        .find(|s| s.name == "SystemSymbolHintsStream")
        .expect("a name, not a hex number");
    assert_eq!(stream.vendor, "DTB KE");
    assert!(stream.understood);

    let text = rawdump::stream_text(&d, stream.type_id);
    for expected in [
        "macos-dyld-cache",
        "exact",
        "no — skipped",
        "71 of 93 system-library frames named (22 without a name)",
        "26A428",
        "3 functions in 2 images",
        "── libx.dylib",
        "(image not in the module list)",
        "_orphan",
    ] {
        assert!(text.contains(expected), "missing {expected:?} in:\n{text}");
    }
    // Entries within an image are ordered by address.
    assert!(
        text.find("_first").unwrap() < text.find("_second").unwrap(),
        "{text}"
    );
    assert!(!text.contains("hex dump"));
}

#[test]
fn a_damaged_system_symbol_stream_says_so_and_falls_back_to_hex() {
    let d = dump_with_system_hints(b"not a hints stream at all");
    let list = rawdump::streams(&d);
    let stream = list
        .iter()
        .find(|s| s.name == "SystemSymbolHintsStream")
        .unwrap();
    let text = rawdump::stream_text(&d, stream.type_id);
    assert!(text.contains("not a system-symbol stream"), "{text}");
    assert!(text.contains("hex dump"), "{text}");
}
