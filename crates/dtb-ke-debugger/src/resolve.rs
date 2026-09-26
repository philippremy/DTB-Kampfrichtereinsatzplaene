//! Walk a dump's modules through the configured sources and record what was found — and, for the UI,
//! what was tried when nothing was.

use std::sync::Arc;

use crate::build::BuildInfo;
use crate::identity::ModuleRef;
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

/// An ordered chain of sources; the first hit wins. Order them cheapest / most local first
/// (executable → local files → cache → symbol server).
#[derive(Default, Clone)]
pub struct Resolver {
    sources: Vec<Arc<dyn DebugFileSource>>,
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with(mut self, source: impl DebugFileSource + 'static) -> Self {
        self.sources.push(Arc::new(source));
        self
    }

    pub fn push(&mut self, source: Arc<dyn DebugFileSource>) {
        self.sources.push(source);
    }

    /// Resolve one module. `progress` is told which source is being asked, for the live status line.
    pub async fn resolve_module(
        &self,
        module: &ModuleRef,
        build: Option<&BuildInfo>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> Outcome {
        let mut tried = Vec::new();
        if module.debug_id.is_none() {
            return Outcome::Missing {
                tried: vec!["the dump records no debug id for this module".into()],
            };
        }
        for source in &self.sources {
            let name = source.name();
            progress(&format!("{}: {name}", module.short_name()));
            match source.find(module, build).await {
                Ok(Some(found)) => {
                    log::info!(
                        "resolved {} via {name}: {}",
                        module.short_name(),
                        found.path.display()
                    );
                    return Outcome::Found(found);
                }
                Ok(None) => tried.push(format!("{name}: not found")),
                Err(err) => {
                    log::warn!("{name} failed for {}: {err:#}", module.short_name());
                    tried.push(format!("{name}: {err:#}"));
                }
            }
        }
        Outcome::Missing { tried }
    }

    pub async fn resolve(
        &self,
        modules: &[ModuleRef],
        build: Option<&BuildInfo>,
        progress: &(dyn Fn(&str) + Sync),
    ) -> Resolution {
        let mut out = Resolution::default();
        for module in modules {
            let outcome = self.resolve_module(module, build, progress).await;
            out.modules.push(ModuleResolution {
                module: module.clone(),
                outcome,
            });
        }
        out
    }
}
