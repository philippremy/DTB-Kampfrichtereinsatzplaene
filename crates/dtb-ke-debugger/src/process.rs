//! Open → resolve → symbolicate → stackwalk, as one call for the viewer and for tests.

use std::path::Path;

use memmap2::Mmap;
use minidump::Minidump;
use minidump_processor::ProcessState;

use std::sync::Arc;

use dtb_ke_crash::syshints::{self, Hints};

use crate::build::BuildInfo;
use crate::discover::SystemFacts;
use crate::identity::{ModuleRef, modules_of};
use crate::progress::Progress;
use crate::resolve::{Resolution, Resolver};
use crate::symbolize::{NativeSymbolProvider, NoSymbols};

pub struct OpenedDump {
    pub dump: Minidump<'static, Mmap>,
    /// The OS the dump ran on — its build names the per-build symbol material to look for.
    pub system: SystemFacts,
    pub build: Option<BuildInfo>,
    /// The system-symbol hints the crashed machine wrote into the dump, if any (`dtb-ke-crash::syshints`).
    pub hints: Option<Arc<Hints>>,
    pub modules: Vec<ModuleRef>,
}

impl OpenedDump {
    pub fn open(path: &Path) -> anyhow::Result<Self> {
        let dump = Minidump::read_path(path)?;
        let build = BuildInfo::from_dump(&dump);
        let mut modules = modules_of(&dump);
        // The main module's recorded path is unusable on its own: macOS dumps leave it empty, and an app started
        // through a relative path (`cargo run` → `target/debug/app`) records that relative path, which only
        // resolves from one working directory. The build-info stream carries the absolute one.
        if let (Some(main), Some(exe)) = (
            modules.first_mut(),
            build.as_ref().and_then(BuildInfo::exe_path),
        ) {
            if main.code_file.is_empty() || !Path::new(&main.code_file).is_absolute() {
                main.code_file = exe.to_owned();
            }
        }
        let system = dump
            .get_stream::<minidump::MinidumpSystemInfo>()
            .map(|s| SystemFacts {
                os: s.os.to_string(),
                version: format!("{}.{}", s.raw.major_version, s.raw.minor_version),
                // Apple dumps keep the build (`26A428`) in the CSD string; elsewhere it is free text.
                build: s
                    .csd_version()
                    .map(|b| b.trim().to_owned())
                    .filter(|b| b.len() <= 12 && !b.contains(' '))
                    .unwrap_or_default(),
            })
            .unwrap_or_default();
        let hints = dump
            .get_raw_stream(syshints::STREAM_TYPE)
            .ok()
            .and_then(|raw| Hints::parse(&String::from_utf8_lossy(raw)))
            .map(Arc::new);
        Ok(Self {
            dump,
            system,
            build,
            hints,
            modules,
        })
    }
}

pub struct Analysis {
    pub state: ProcessState,
    pub resolution: Resolution,
}

/// The four stages `analyze` reports to its [`Progress`].
pub const STAGES: usize = 4;

/// Open → resolve → load symbols → unwind + symbolicate, reporting each stage (with an `n / m` where it can be
/// counted) to `progress`. Every stage but the unwind is countable: modules, modules, then frames — the unwind is one
/// short call without a count, and its result is what gives the last stage its total.
pub async fn analyze(
    opened: &OpenedDump,
    resolver: &Resolver,
    progress: &Arc<Progress>,
) -> anyhow::Result<Analysis> {
    progress.begin_stage(
        1,
        STAGES,
        "Finding debug files",
        opened.modules.len() as u64,
    );
    let resolution = resolver
        .with_progress(progress.clone())
        .resolve(&opened.modules, opened.build.as_ref(), &|_| {})
        .await;

    progress.begin_stage(
        2,
        STAGES,
        "Loading symbols",
        resolution.found().count() as u64,
    );
    let provider =
        NativeSymbolProvider::load_with(&resolution, opened.hints.clone(), Some(progress.clone()))
            .await;

    // Unwind once without symbols to learn how many frames there are …
    progress.begin_stage(3, STAGES, "Unwinding stacks", 0);
    let unwound = minidump_processor::process_minidump(&opened.dump, &NoSymbols).await?;
    let frames: u64 = unwound.threads.iter().map(|t| t.frames.len() as u64).sum();
    drop(unwound);

    // … so the pass that symbolicates them can count.
    progress.begin_stage(4, STAGES, "Symbolicating frames", frames);
    let state = minidump_processor::process_minidump(&opened.dump, &provider).await?;
    progress.finish_stage();
    Ok(Analysis { state, resolution })
}
