//! Headless run of the whole pipeline: `cargo run -p dtb-ke-debugger --example analyze -- <dump.dmp> [symbol-dir-or-file…]`

use dtb_ke_debugger::process::{OpenedDump, analyze};
use dtb_ke_debugger::{DirectorySource, ExecutableSource, Outcome, Resolver};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let mut args = std::env::args().skip(1);
    let dump = args
        .next()
        .ok_or_else(|| anyhow::anyhow!("usage: analyze <dump.dmp> [symbols…]"))?;
    let opened = OpenedDump::open(std::path::Path::new(&dump))?;

    println!("system: {}", opened.system);
    let found = dtb_ke_debugger::discover(&opened.system);
    println!("discovered: {found:?}");

    match &opened.build {
        Some(b) => {
            println!("build info:");
            for (k, v) in &b.pairs {
                println!("  {k} = {v}");
            }
        }
        None => println!("build info: none in this dump"),
    }

    let mut resolver = Resolver::new()
        .with(ExecutableSource)
        .with(dtb_ke_debugger::DyldSharedCacheSource);
    let extra: Vec<_> = args.map(std::path::PathBuf::from).collect();
    if !extra.is_empty() {
        resolver = resolver.with(DirectorySource::new(extra));
    }

    let analysis = analyze(&opened, &resolver, &|s| eprintln!("… {s}")).await?;

    println!("\nmodules:");
    for m in &analysis.resolution.modules {
        match &m.outcome {
            Outcome::Found(f) => println!("  ok   {:<40} {}", m.module.short_name(), f.origin),
            Outcome::Missing { .. } => println!("  --   {}", m.module.short_name()),
        }
    }

    // `ALL_THREADS=1` prints every thread, not just the crashing one.
    let crashed = analysis.state.requesting_thread.unwrap_or(0);
    let all = std::env::var_os("ALL_THREADS").is_some();
    for (t, stack) in analysis.state.threads.iter().enumerate() {
        if !all && t != crashed {
            continue;
        }
        println!(
            "\nthread {t}{} ({} frames):",
            if t == crashed { " [crashed]" } else { "" },
            stack.frames.len()
        );
        for (i, f) in stack.frames.iter().take(25).enumerate() {
            let module = f
                .module
                .as_ref()
                .map(|m| m.name.rsplit('/').next().unwrap_or(&m.name))
                .unwrap_or("?");
            let func = f.function_name.as_deref().unwrap_or("<no symbol>");
            let src = match (&f.source_file_name, f.source_line) {
                (Some(file), Some(line)) => format!("{file}:{line}"),
                _ => String::new(),
            };
            println!("  #{i:<2} {:?} {module} {func} {src}", f.trust);
        }
    }
    Ok(())
}
