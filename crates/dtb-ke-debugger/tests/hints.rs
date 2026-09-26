//! The crashed machine's system-symbol hints (a dump user stream) as the fallback for system modules nothing
//! else located: the frame gets the hint's name, approximate hints are marked `≈`, and an exact / complete stream
//! is read back as such.

use dtb_ke_crash::snapshot::{
    Arm64RegsDTO, CrashSnapshotDTO, ModuleDTO, SessionDTO, ThreadDTO, to_minidump,
};
use dtb_ke_crash::syshints::{Entry, Hints, Quality, STREAM_TYPE};
use dtb_ke_debugger::Resolver;
use dtb_ke_debugger::process::{OpenedDump, analyze};

const LIB_UUID: [u8; 16] = [0x42; 16];
const BASE: u64 = 0x1_8000_0000;

/// A dump whose crashing thread sits `0x120` bytes into the system library `libx.dylib`.
fn dump_with(hints: Option<&Hints>) -> std::path::PathBuf {
    let mut regs = Arm64RegsDTO::default();
    regs.pc = BASE + 0x120;
    let snap = CrashSnapshotDTO {
        threads: vec![ThreadDTO {
            id: 7,
            crashed: true,
            name: "main".into(),
            regs,
            stack: None,
        }],
        session: SessionDTO {
            modules: vec![ModuleDTO {
                base: BASE,
                size: 0x10_0000,
                uuid: LIB_UUID,
                is_main: false,
                path: "/usr/lib/libx.dylib".into(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bytes = to_minidump(&snap);
    if let Some(h) = hints {
        dtb_ke_crash::patch::append_stream(&mut bytes, STREAM_TYPE, h.encode().as_bytes()).unwrap();
    }
    let path = std::env::temp_dir().join(format!(
        "dtbke-hints-{}-{}.dmp",
        std::process::id(),
        h_id(hints)
    ));
    std::fs::write(&path, bytes).unwrap();
    path
}

fn h_id(h: Option<&Hints>) -> u8 {
    match h {
        None => 0,
        Some(h) if h.quality == Quality::Exact => 1,
        Some(_) => 2,
    }
}

fn hints(quality: Quality, complete: bool) -> Hints {
    Hints {
        producer: "test".into(),
        quality,
        complete,
        incomplete_reason: if complete {
            String::new()
        } else {
            "skipped".into()
        },
        frames_total: 1,
        frames_resolved: 1,
        os_build: "TESTBUILD".into(),
        entries: vec![Entry {
            uuid: LIB_UUID,
            start: 0x100,
            end: 0x140,
            name: "-[NSThing run]".into(),
        }],
    }
}

async fn top_frame_name(path: &std::path::Path) -> Option<String> {
    let opened = OpenedDump::open(path).unwrap();
    let analysis = analyze(&opened, &Resolver::new(), &|_| {}).await.unwrap();
    analysis.state.threads[0].frames[0].function_name.clone()
}

#[tokio::test]
async fn exact_hints_name_an_unlocated_system_frame() {
    let path = dump_with(Some(&hints(Quality::Exact, true)));
    let opened = OpenedDump::open(&path).unwrap();
    let h = opened.hints.as_ref().expect("stream read back");
    assert!(h.complete && h.quality == Quality::Exact);
    assert_eq!(
        top_frame_name(&path).await.as_deref(),
        Some("-[NSThing run]")
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn approximate_hints_are_marked() {
    let path = dump_with(Some(&hints(Quality::Approximate, false)));
    let opened = OpenedDump::open(&path).unwrap();
    let h = opened.hints.as_ref().unwrap();
    assert!(!h.complete && h.incomplete_reason == "skipped" && h.quality == Quality::Approximate);
    assert_eq!(
        top_frame_name(&path).await.as_deref(),
        Some("≈ -[NSThing run]")
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn without_hints_the_frame_stays_unnamed() {
    let path = dump_with(None);
    assert!(OpenedDump::open(&path).unwrap().hints.is_none());
    assert_eq!(top_frame_name(&path).await, None);
    let _ = std::fs::remove_file(path);
}
