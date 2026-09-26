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
    let path = std::env::temp_dir().join(format!("dtbke-rel-{}.dmp", std::process::id()));
    std::fs::write(&path, to_minidump(&snap)).unwrap();
    let opened = dtb_ke_debugger::process::OpenedDump::open(&path).unwrap();
    let _ = std::fs::remove_file(&path);
    assert_eq!(opened.modules[0].code_file, "/abs/target/debug/app");
}
