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
use crate::resolve::{Resolution, Resolver};
use crate::symbolize::NativeSymbolProvider;

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

pub async fn analyze(
    opened: &OpenedDump,
    resolver: &Resolver,
    progress: &(dyn Fn(&str) + Sync),
) -> anyhow::Result<Analysis> {
    let resolution = resolver
        .resolve(&opened.modules, opened.build.as_ref(), progress)
        .await;
    progress("symbolicating");
    let provider = NativeSymbolProvider::load(&resolution, opened.hints.clone()).await;
    progress("walking stacks");
    let state = minidump_processor::process_minidump(&opened.dump, &provider).await?;
    Ok(Analysis { state, resolution })
}
