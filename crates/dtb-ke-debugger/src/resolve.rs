//! Walk a dump's modules through the configured sources and record what was found — and, for the UI,
//! what was tried when nothing was.

use std::sync::Arc;

use crate::build::BuildInfo;
use crate::identity::ModuleRef;
use crate::progress::Progress;
use crate::source::{DebugFileSource, FoundFile};

pub enum Outcome {
    Found(FoundFile),
    /// Every source said "not here" (or failed); `tried` is one line per source for the Symbole panel.
    Missing {
        tried: Vec<String>,
    },
}

pub struct ModuleResolution {
    pub module: ModuleRef,
    pub outcome: Outcome,
    /// What the sources that did **not** provide the file used said (`"symbol server (…): not found"`, an HTTP
    /// error …). Empty when the first source answered with debug info. For a module that only got a symbol table,
    /// this is why nothing better turned up.
    pub notes: Vec<String>,
}

#[derive(Default)]
pub struct Resolution {
    pub modules: Vec<ModuleResolution>,
}

impl Resolution {
    pub fn found(&self) -> impl Iterator<Item = (&ModuleRef, &FoundFile)> {
        self.modules.iter().filter_map(|m| match &m.outcome {
            Outcome::Found(f) => Some((&m.module, f)),
            Outcome::Missing { .. } => None,
        })
    }

    pub fn for_module(&self, base: u64) -> Option<&ModuleResolution> {
        self.modules.iter().find(|m| m.module.base == base)
    }
}

/// An ordered chain of sources. The first hit **with debug info** wins; a hit that carries only a symbol table
/// (typically the crashed build's own stripped executable, found first because it is on this machine) is kept as
/// the fallback while the later sources — the download cache, the symbol server — get their chance at a better
/// file. Order them cheapest / most local first (executable → local files → cache → symbol server).
#[derive(Default, Clone)]
pub struct Resolver {
    sources: Vec<Arc<dyn DebugFileSource>>,
    /// Told which module is being looked up and when it is done (for the `n / m` bar).
    progress: Option<Arc<Progress>>,
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, source: impl DebugFileSource + 'static) -> Self {
        self.sources.push(Arc::new(source));
        self
    }

    /// A copy of this resolver that reports each module to `progress` (one `advance` per module).
    pub fn with_progress(&self, progress: Arc<Progress>) -> Self {
        Self {
            progress: Some(progress),
            ..self.clone()
        }
    }

    pub fn push(&mut self, source: Arc<dyn DebugFileSource>) {
        self.sources.push(source);
    }

    /// Resolve one module. `progress` is told which source is being asked, for the live status line. Returns the
    /// outcome plus the notes of every consulted source that did not provide the file used.
    pub async fn resolve_module(
        &self,
        module: &ModuleRef,
        build: Option<&BuildInfo>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> (Outcome, Vec<String>) {
        let mut notes = Vec::new();
        if module.debug_id.is_none() {
            let why = "the dump records no debug id for this module".to_string();
            return (
                Outcome::Missing {
                    tried: vec![why.clone()],
                },
                vec![why],
            );
        }
        let mut fallback: Option<FoundFile> = None;
        for source in &self.sources {
            let name = source.name();
            progress(&format!("{}: {name}", module.short_name()));
            match source.find(module, build).await {
                Ok(Some(found)) if found.has_debug_info || found.in_dyld_cache => {
                    log::info!(
                        "resolved {} via {name}: {}",
                        module.short_name(),
                        found.path.display()
                    );
                    return (Outcome::Found(found), notes);
                }
                Ok(Some(found)) => {
                    log::debug!(
                        "{} via {name} has only a symbol table — looking for better",
                        module.short_name()
                    );
                    // The first symbol-table-only hit is kept, unless a later one at least has real names.
                    if fallback
                        .as_ref()
                        .is_none_or(|f| found.has_symbols && !f.has_symbols)
                    {
                        fallback = Some(found);
                    }
                }
                Ok(None) => notes.push(format!("{name}: not found")),
                Err(err) => {
                    log::warn!("{name} failed for {}: {err:#}", module.short_name());
                    notes.push(format!("{name}: {err:#}"));
                }
            }
        }
        match fallback {
            Some(found) => {
                log::info!(
                    "resolved {} with a symbol table only: {}",
                    module.short_name(),
                    found.path.display()
                );
                (Outcome::Found(found), notes)
            }
            None => (
                Outcome::Missing {
                    tried: notes.clone(),
                },
                notes,
            ),
        }
    }

    pub async fn resolve(
        &self,
        modules: &[ModuleRef],
        build: Option<&BuildInfo>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> Resolution {
        let mut out = Resolution::default();
        for module in modules {
            if let Some(p) = &self.progress {
                p.set_label(module.short_name());
            }
            let (outcome, notes) = self.resolve_module(module, build, progress).await;
            if let Some(p) = &self.progress {
                p.advance(1);
            }
            out.modules.push(ModuleResolution {
                module: module.clone(),
                outcome,
                notes,
            });
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use std::path::PathBuf;

    /// A source with a canned answer.
    struct Fixed(&'static str, Option<bool>);

    #[async_trait]
    impl DebugFileSource for Fixed {
        fn name(&self) -> String {
            self.0.into()
        }
        async fn find(
            &self,
            _: &ModuleRef,
            _: Option<&BuildInfo>,
        ) -> anyhow::Result<Option<FoundFile>> {
            Ok(self.1.map(|has_debug_info| FoundFile {
                path: PathBuf::from(self.0),
                has_debug_info,
                has_symbols: true,
                origin: self.0.into(),
                in_dyld_cache: false,
            }))
        }
    }

    fn module() -> ModuleRef {
        ModuleRef {
            base: 0,
            size: 0,
            code_file: "/x/app".into(),
            debug_file: None,
            debug_id: Some(samply_symbols::debugid::DebugId::nil()),
        }
    }

    async fn resolve(sources: Vec<Fixed>) -> (Outcome, Vec<String>) {
        let mut r = Resolver::new();
        for s in sources {
            r = r.with(s);
        }
        r.resolve_module(&module(), None, &|_| {}).await
    }

    #[tokio::test]
    async fn a_later_source_with_debug_info_beats_an_earlier_symbol_table() {
        let (outcome, notes) = resolve(vec![
            Fixed("exe", Some(false)),
            Fixed("cache", None),
            Fixed("server", Some(true)),
        ])
        .await;
        let Outcome::Found(f) = outcome else {
            panic!("not found")
        };
        assert_eq!(f.origin, "server");
        assert_eq!(notes, ["cache: not found"]);
    }

    #[tokio::test]
    async fn the_symbol_table_is_the_fallback_and_records_why_nothing_better_came() {
        let (outcome, notes) =
            resolve(vec![Fixed("exe", Some(false)), Fixed("server", None)]).await;
        let Outcome::Found(f) = outcome else {
            panic!("not found")
        };
        assert_eq!(f.origin, "exe");
        assert!(!f.has_debug_info);
        assert_eq!(notes, ["server: not found"]);
    }

    #[tokio::test]
    async fn the_first_source_with_debug_info_still_wins_at_once() {
        let (outcome, notes) =
            resolve(vec![Fixed("dsym", Some(true)), Fixed("server", Some(true))]).await;
        let Outcome::Found(f) = outcome else {
            panic!("not found")
        };
        assert_eq!(f.origin, "dsym");
        assert!(notes.is_empty());
    }

    #[tokio::test]
    async fn nothing_anywhere_is_missing() {
        let (outcome, _) = resolve(vec![Fixed("a", None), Fixed("b", None)]).await;
        let Outcome::Missing { tried } = outcome else {
            panic!("found from nothing")
        };
        assert_eq!(tried, ["a: not found", "b: not found"]);
    }
}
