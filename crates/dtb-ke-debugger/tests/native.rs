//! The resolve → symbolicate chain against real files: this test binary is its own fixture.

use dtb_ke_debugger::identity::identify;
use dtb_ke_debugger::symbolize::NativeSymbolProvider;
use dtb_ke_debugger::{ExecutableSource, ModuleRef, Resolver};
use minidump::MinidumpModule;
use minidump_unwind::{FrameSymbolizer, SymbolProvider};

#[inline(never)]
#[unsafe(no_mangle)]
pub extern "C" fn dtb_ke_debugger_test_marker(x: u64) -> u64 {
    std::hint::black_box(x).wrapping_mul(31).wrapping_add(7)
}

#[derive(Default)]
struct Capture {
    ip: u64,
    function: Option<String>,
    file: Option<(String, u32)>,
}

impl FrameSymbolizer for Capture {
    fn get_instruction(&self) -> u64 {
        self.ip
    }
    fn set_function(&mut self, name: &str, _base: u64, _size: u32) {
        self.function = Some(name.to_owned());
    }
    fn set_source_file(&mut self, file: &str, line: u32, _base: u64) {
        self.file = Some((file.to_owned(), line));
    }
}

/// The loaded image containing `addr`: `(base, path)` via dladdr.
fn image_of(addr: usize) -> (u64, String) {
    unsafe {
        let mut info: libc::Dl_info = std::mem::zeroed();
        assert_ne!(libc::dladdr(addr as *const _, &mut info), 0);
        let path = std::ffi::CStr::from_ptr(info.dli_fname)
            .to_string_lossy()
            .into_owned();
        (info.dli_fbase as u64, path)
    }
}

#[test]
fn own_executable_has_an_identity() {
    let exe = std::env::current_exe().unwrap();
    let ids = identify(&exe);
    assert!(!ids.is_empty(), "no identity for {}", exe.display());
}

#[tokio::test]
async fn symbolicates_a_function_in_the_running_binary() {
    let addr = dtb_ke_debugger_test_marker as *const () as usize;
    let (base, path) = image_of(addr);
    let id = identify(std::path::Path::new(&path))
        .into_iter()
        .next()
        .expect("identity")
        .debug_id;

    let module = MinidumpModule::new(base, 0x0100_0000, &path);
    let module_ref = ModuleRef {
        debug_id: Some(id),
        ..ModuleRef::from_module(&module)
    };

    let resolution = Resolver::new()
        .with(ExecutableSource)
        .resolve(&[module_ref], None, &|_| {})
        .await;
    assert_eq!(
        resolution.found().count(),
        1,
        "the executable must resolve to itself"
    );

    let provider = NativeSymbolProvider::load(&resolution, None).await;
    assert_eq!(provider.symbolicated_modules(), 1);

    let mut frame = Capture {
        ip: addr as u64 + 4,
        ..Default::default()
    };
    provider
        .fill_symbol(&module, &mut frame)
        .await
        .expect("symbols");
    let function = frame.function.expect("function name");
    assert!(
        function.contains("dtb_ke_debugger_test_marker"),
        "got {function:?}"
    );
    let (file, line) = frame.file.expect("source location");
    assert!(file.ends_with("native.rs"), "got {file}");
    assert!(line > 0);
}

/// The status a user sees for the crashed executable of a local `cargo run` build. On macOS such an executable has no
/// DWARF of its own (a debug map points at the object files), so judging it by its file alone said "symbols only" even
/// though every frame had a source line. The status must follow what the frames actually got.
#[tokio::test]
async fn a_local_debug_build_is_debug_info_in_the_analysis() {
    use dtb_ke_crash::snapshot::{
        Arm64RegsDTO, CrashSnapshotDTO, ModuleDTO, SessionDTO, ThreadDTO, to_minidump,
    };
    use dtb_ke_debugger::process::{OpenedDump, analyze};
    use dtb_ke_debugger::progress::Progress;
    use dtb_ke_debugger::quality::Quality;

    let addr = dtb_ke_debugger_test_marker as *const () as usize;
    let (base, path) = image_of(addr);
    let id = identify(std::path::Path::new(&path))
        .into_iter()
        .next()
        .expect("identity")
        .debug_id;

    let snap = CrashSnapshotDTO {
        threads: vec![ThreadDTO {
            id: 1,
            crashed: true,
            name: "main".into(),
            regs: Arm64RegsDTO {
                pc: addr as u64 + 4,
                ..Default::default()
            },
            stack: None,
        }],
        session: SessionDTO {
            modules: vec![ModuleDTO {
                base,
                size: 0x0100_0000,
                uuid: *id.uuid().as_bytes(),
                is_main: true,
                path: path.clone(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let dump = std::env::temp_dir().join(format!("dtbke-quality-{}.dtbkedmp", std::process::id()));
    std::fs::write(&dump, to_minidump(&snap)).unwrap();
    let opened = OpenedDump::open(&dump).unwrap();
    let _ = std::fs::remove_file(&dump);

    let resolver = Resolver::new().with(ExecutableSource);
    let analysis = analyze(&opened, &resolver, &Progress::new()).await.unwrap();

    let module = analysis.resolution.for_module(base).expect("the module");
    let usage = analysis.usage.get(&base).expect("a frame sits in it");
    assert!(
        usage.with_lines >= 1,
        "the frame must have a source line: {usage:?}"
    );
    assert_eq!(
        analysis.quality(module),
        Quality::DebugInfo,
        "usage: {usage:?}"
    );
}
