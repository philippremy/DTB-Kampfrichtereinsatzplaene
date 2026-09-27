//! The progress bars' numbers are real: the analysis stages count what they claim to, the frame total of the last stage
//! is exactly what the symbolicating pass is asked about, and a download reports bytes while it is running.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use dtb_ke_crash::snapshot::{
    Arm64RegsDTO, CrashSnapshotDTO, ModuleDTO, SessionDTO, ThreadDTO, to_minidump,
};
use dtb_ke_debugger::identity::identify;
use dtb_ke_debugger::process::{OpenedDump, analyze};
use dtb_ke_debugger::progress::Progress;
use dtb_ke_debugger::remote::{CacheSource, SymbolServerSource};
use dtb_ke_debugger::symbolize::NoSymbols;
use dtb_ke_debugger::{ModuleRef, Outcome, Resolver};
use minidump_unwind::{
    FileError, FileKind, FillSymbolError, FrameSymbolizer, FrameWalker, SymbolProvider,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A dump with two threads of one and two frames (fp-walked from a small fake stack is overkill: one frame each is
/// what the synthetic assembler gives, which is enough to count).
fn dump() -> std::path::PathBuf {
    let thread = |id: u32, pc: u64| {
        let regs = Arm64RegsDTO {
            pc,
            ..Default::default()
        };
        ThreadDTO {
            id: u64::from(id),
            crashed: id == 1,
            name: format!("t{id}"),
            regs,
            stack: None,
        }
    };
    let snap = CrashSnapshotDTO {
        threads: vec![thread(1, 0x1_8000_0100), thread(2, 0x1_8000_0200)],
        session: SessionDTO {
            modules: vec![ModuleDTO {
                base: 0x1_8000_0000,
                size: 0x10_0000,
                uuid: [7; 16],
                is_main: false,
                path: "/usr/lib/libx.dylib".into(),
            }],
            ..Default::default()
        },
        ..Default::default()
    };
    let path = std::env::temp_dir().join(format!("dtbke-progress-{}.dtbkedmp", std::process::id()));
    std::fs::write(&path, to_minidump(&snap)).unwrap();
    path
}

/// Counts how often the processor asks for a frame's symbols.
struct Counting(Arc<AtomicUsize>);

#[async_trait]
impl SymbolProvider for Counting {
    async fn fill_symbol(
        &self,
        _m: &(dyn minidump::Module + Sync),
        _f: &mut (dyn FrameSymbolizer + Send),
    ) -> Result<(), FillSymbolError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(FillSymbolError {})
    }
    async fn walk_frame(
        &self,
        _m: &(dyn minidump::Module + Sync),
        _w: &mut (dyn FrameWalker + Send),
    ) -> Option<()> {
        None
    }
    async fn get_file_path(
        &self,
        _m: &(dyn minidump::Module + Sync),
        _k: FileKind,
    ) -> Result<std::path::PathBuf, FileError> {
        Err(FileError::NotFound)
    }
}

#[tokio::test]
async fn the_symbol_free_pass_counts_exactly_the_frames_the_symbolicating_pass_asks_about() {
    let path = dump();
    let opened = OpenedDump::open(&path).unwrap();
    let unwound = minidump_processor::process_minidump(&opened.dump, &NoSymbols)
        .await
        .unwrap();
    let frames: usize = unwound.threads.iter().map(|t| t.frames.len()).sum();
    assert!(frames >= 2, "the fixture has at least one frame per thread");

    let calls = Arc::new(AtomicUsize::new(0));
    minidump_processor::process_minidump(&opened.dump, &Counting(calls.clone()))
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        frames,
        "one fill_symbol call per frame, no more, no less"
    );
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn the_analysis_ends_on_a_full_last_stage_with_the_frame_count() {
    let path = dump();
    let opened = OpenedDump::open(&path).unwrap();
    let progress = Progress::new();
    analyze(&opened, &Resolver::new(), &progress).await.unwrap();
    let s = progress.snapshot();
    assert_eq!((s.stage_no, s.stage_of), (4, 4));
    assert_eq!(s.stage, "Symbolicating frames");
    assert!(
        s.total >= 2,
        "the total is the frame count from the unwind pass: {s:?}"
    );
    assert_eq!(s.fraction(), Some(1.0));
    let _ = std::fs::remove_file(path);
}

/// Serves `body` for any request in slow chunks, with a real `Content-Length`.
async fn slow_server(body: Vec<u8>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let body = body.clone();
            tokio::spawn(async move {
                let mut buf = vec![0u8; 8192];
                let _ = sock.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                for chunk in body.chunks(body.len().div_ceil(8)) {
                    let _ = sock.write_all(chunk).await;
                    let _ = sock.flush().await;
                    tokio::time::sleep(Duration::from_millis(60)).await;
                }
            });
        }
    });
    base
}

#[tokio::test]
async fn a_download_reports_its_bytes_while_it_runs_and_leaves_no_scratch_file() {
    let exe = std::env::current_exe().unwrap();
    let id = identify(&exe)
        .into_iter()
        .next()
        .expect("identity")
        .debug_id;
    let module = ModuleRef {
        base: 0x1000,
        size: 0x1000,
        code_file: "/no/such/place/app".into(),
        debug_file: Some("app".into()),
        debug_id: Some(id),
    };
    let bytes = std::fs::read(&exe).unwrap();
    let len = bytes.len() as u64;
    let base = slow_server(bytes).await;

    let cache_dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(CacheSource::new(cache_dir.path().to_owned()));
    let progress = Progress::new();
    let resolver = Resolver::new().with(
        SymbolServerSource::new(&base, None, cache)
            .unwrap()
            .with_progress(progress.clone()),
    );

    // Poll while the resolver downloads.
    let finished = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let poller = {
        let (progress, finished) = (progress.clone(), finished.clone());
        tokio::spawn(async move {
            let mut seen = Vec::new();
            // The bound is a backstop: the flag ends the loop, but a test must never be able to hang the suite.
            for _ in 0..400 {
                if finished.load(Ordering::SeqCst) {
                    break;
                }
                if let Some(t) = progress.snapshot().transfer {
                    seen.push((t.done, t.total));
                }
                tokio::time::sleep(Duration::from_millis(15)).await;
            }
            seen
        })
    };
    let res = resolver
        .resolve(std::slice::from_ref(&module), None, &|_| {})
        .await;
    finished.store(true, Ordering::SeqCst);
    assert!(
        matches!(res.modules[0].outcome, Outcome::Found(_)),
        "the download must still verify and resolve"
    );
    let seen = poller.await.unwrap();

    assert!(
        seen.iter().all(|(_, total)| *total == Some(len)),
        "the total is the Content-Length: {seen:?}"
    );
    assert!(
        seen.iter().any(|(done, _)| *done > 0 && *done < len),
        "at least one mid-transfer sample, not just 0 and done: {seen:?}"
    );
    assert!(
        seen.windows(2).all(|w| w[0].0 <= w[1].0),
        "bytes never go backwards: {seen:?}"
    );
    assert!(
        progress.snapshot().transfer.is_none(),
        "the transfer ends with the download"
    );

    let leftovers: Vec<_> = std::fs::read_dir(cache_dir.path())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|e| e.file_name().to_string_lossy().starts_with(".download-"))
        .collect();
    assert!(leftovers.is_empty(), "scratch files remain: {leftovers:?}");
}
