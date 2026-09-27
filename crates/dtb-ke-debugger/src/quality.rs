//! What the debugger actually got for each module — the status pill on the Symbols tab and the symbol-server hint.
//!
//! A file's own properties are not enough to say that: a local debug build's executable has no DWARF of its own (the
//! symbolizer follows a debug map to the object files), and a stripped binary still defines a few functions for the
//! dynamic linker while every frame in it ends up as a synthesized `fun_<address>`. So the classification looks at
//! **what the module's frames received** when the stacks were symbolicated, and falls back to the file's properties
//! only for a module no frame sits in.
//!
//! | [`Quality`] | pill | when |
//! |---|---|---|
//! | `DebugInfo` | `debug info` | a frame in the module got a source file **and** line — or, with no frame in it, the file has debug info |
//! | `SymbolsOnly` | `symbols only` | frames got real function names but no source lines — or, with no frame in it, the file defines named functions |
//! | `NoSymbols` | `no symbols` | a file was found, but its frames got no name or only synthesized `fun_<address>` ones — or, with no frame in it, the file has neither debug info nor named functions |
//! | `FromDump` | `from crash report` | no file was found, but frames got names from the dump's own system-symbol stream |
//! | `Missing` | `missing` | no file was found and no frame got a name |

use std::collections::HashMap;

use minidump::Module;
use minidump_processor::ProcessState;

use crate::resolve::Outcome;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    DebugInfo,
    SymbolsOnly,
    NoSymbols,
    FromDump,
    Missing,
}

impl Quality {
    /// The pill's text.
    pub fn label(self) -> &'static str {
        match self {
            Quality::DebugInfo => "debug info",
            Quality::SymbolsOnly => "symbols only",
            Quality::NoSymbols => "no symbols",
            Quality::FromDump => "from crash report",
            Quality::Missing => "missing",
        }
    }

    /// One line on what the status means for the frames in this module (empty when there is nothing to warn about).
    pub fn explanation(self) -> &'static str {
        match self {
            Quality::DebugInfo => "",
            Quality::SymbolsOnly => "function names only, no source lines",
            Quality::NoSymbols => {
                "stripped: no function names (frames show synthesized fun_<address>), no source lines"
            }
            Quality::FromDump => {
                "no debug file located; function names come from the crash report's own system-symbol stream"
            }
            Quality::Missing => "",
        }
    }

    /// The module has real symbol names for at least some frames.
    pub fn has_names(self) -> bool {
        matches!(
            self,
            Quality::DebugInfo | Quality::SymbolsOnly | Quality::FromDump
        )
    }
}

/// What the frames of one module received.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModuleUsage {
    /// Frames (in any thread) whose instruction lies in the module.
    pub frames: u32,
    /// … that got a real function name.
    pub named: u32,
    /// … whose only name is a synthesized `fun_<address>` (function starts known, names not).
    pub synthesized: u32,
    /// … that got a source file and line (own or of an inlined frame).
    pub with_lines: u32,
}

impl ModuleUsage {
    /// `5 frames: 4 with source lines, 5 named`, for the row's detail line.
    pub fn summary(&self) -> String {
        let mut parts = vec![format!(
            "{} frame{}",
            self.frames,
            if self.frames == 1 { "" } else { "s" }
        )];
        parts.push(format!("{} with source lines", self.with_lines));
        parts.push(format!("{} named", self.named));
        if self.synthesized > 0 {
            parts.push(format!("{} synthesized", self.synthesized));
        }
        format!("{}: {}", parts[0], parts[1..].join(", "))
    }
}

/// `fun_` followed by hex digits only — the name the symbolizer invents for a function it can only place.
pub fn is_synthesized(name: &str) -> bool {
    name.strip_prefix("fun_")
        .is_some_and(|hex| !hex.is_empty() && hex.bytes().all(|b| b.is_ascii_hexdigit()))
}

/// A hint-derived name carries a leading `≈` (approximate); the name itself is what counts.
fn real_name(name: &str) -> bool {
    let name = name.trim_start_matches('≈').trim();
    !name.is_empty() && !is_synthesized(name)
}

/// Per module (by base address): what its frames got.
pub fn usage_of(state: &ProcessState) -> HashMap<u64, ModuleUsage> {
    let mut usage: HashMap<u64, ModuleUsage> = HashMap::new();
    for frame in state.threads.iter().flat_map(|t| &t.frames) {
        let Some(module) = &frame.module else {
            continue;
        };
        let u = usage.entry(module.base_address()).or_default();
        u.frames += 1;
        match frame.function_name.as_deref() {
            Some(name) if real_name(name) => u.named += 1,
            Some(_) => u.synthesized += 1,
            None => {}
        }
        let own_lines = frame.source_file_name.is_some() && frame.source_line.is_some();
        let inlined_lines = frame
            .inlines
            .iter()
            .any(|i| i.source_file_name.is_some() && i.source_line.is_some());
        if own_lines || inlined_lines {
            u.with_lines += 1;
        }
    }
    usage
}

/// The status of one module. `usage` is what its frames got (`None` / zero frames: no frame sits in it).
pub fn classify(outcome: &Outcome, usage: Option<&ModuleUsage>) -> Quality {
    let usage = usage.filter(|u| u.frames > 0);
    match outcome {
        Outcome::Missing { .. } => match usage {
            // Only the dump's own hints can name a frame in a module no file was found for.
            Some(u) if u.named > 0 => Quality::FromDump,
            _ => Quality::Missing,
        },
        Outcome::Found(file) => match usage {
            Some(u) if u.with_lines > 0 => Quality::DebugInfo,
            Some(u) if u.named > 0 => Quality::SymbolsOnly,
            Some(_) => Quality::NoSymbols,
            None if file.has_debug_info => Quality::DebugInfo,
            None if file.has_symbols => Quality::SymbolsOnly,
            None => Quality::NoSymbols,
        },
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::source::FoundFile;

    fn found(has_debug_info: bool, has_symbols: bool) -> Outcome {
        Outcome::Found(FoundFile {
            path: PathBuf::from("/x"),
            has_debug_info,
            has_symbols,
            origin: "test".into(),
            in_dyld_cache: false,
        })
    }

    fn missing() -> Outcome {
        Outcome::Missing { tried: vec![] }
    }

    fn usage(frames: u32, named: u32, synthesized: u32, with_lines: u32) -> ModuleUsage {
        ModuleUsage {
            frames,
            named,
            synthesized,
            with_lines,
        }
    }

    #[test]
    fn synthesized_names_are_recognised_exactly() {
        assert!(is_synthesized("fun_1a2b3c"));
        assert!(is_synthesized("fun_0"));
        assert!(!is_synthesized("fun_"));
        assert!(!is_synthesized("fun_xyz"));
        assert!(!is_synthesized("function_1a2b"));
        assert!(!is_synthesized("run_1a2b3c"));
        assert!(
            !is_synthesized("fun_helper"),
            "a real function that starts with fun_"
        );
        assert!(real_name("≈ _CFRunLoopRun"));
        assert!(!real_name("≈ fun_4f2"));
        assert!(!real_name(""));
    }

    #[test]
    fn what_the_frames_got_decides_when_there_are_frames() {
        // Lines win, whatever the file says — a debug-map executable has no DWARF of its own but the frames have lines.
        assert_eq!(
            classify(&found(false, true), Some(&usage(5, 5, 0, 4))),
            Quality::DebugInfo
        );
        // Names without lines.
        assert_eq!(
            classify(&found(true, true), Some(&usage(3, 3, 0, 0))),
            Quality::SymbolsOnly
        );
        // A file that claims symbols, but every frame is a synthesized fun_… (the stripped external executable).
        assert_eq!(
            classify(&found(false, true), Some(&usage(4, 0, 4, 0))),
            Quality::NoSymbols
        );
        // Nothing at all came out.
        assert_eq!(
            classify(&found(true, true), Some(&usage(2, 0, 0, 0))),
            Quality::NoSymbols
        );
        // Some named, some synthesized: still names.
        assert_eq!(
            classify(&found(false, false), Some(&usage(4, 1, 3, 0))),
            Quality::SymbolsOnly
        );
    }

    #[test]
    fn a_module_without_frames_falls_back_to_the_file() {
        assert_eq!(classify(&found(true, true), None), Quality::DebugInfo);
        assert_eq!(
            classify(&found(true, false), Some(&usage(0, 0, 0, 0))),
            Quality::DebugInfo,
            "zero frames = none"
        );
        assert_eq!(classify(&found(false, true), None), Quality::SymbolsOnly);
        assert_eq!(classify(&found(false, false), None), Quality::NoSymbols);
    }

    #[test]
    fn no_file_is_missing_unless_the_dump_named_the_frames() {
        assert_eq!(classify(&missing(), None), Quality::Missing);
        assert_eq!(
            classify(&missing(), Some(&usage(2, 0, 0, 0))),
            Quality::Missing
        );
        assert_eq!(
            classify(&missing(), Some(&usage(2, 2, 0, 0))),
            Quality::FromDump
        );
        // Synthesized names cannot come from the dump's hints, so they do not make a module "from crash report".
        assert_eq!(
            classify(&missing(), Some(&usage(2, 0, 2, 0))),
            Quality::Missing
        );
    }

    #[test]
    fn labels_and_explanations_exist_for_every_state() {
        for q in [
            Quality::DebugInfo,
            Quality::SymbolsOnly,
            Quality::NoSymbols,
            Quality::FromDump,
            Quality::Missing,
        ] {
            assert!(!q.label().is_empty());
        }
        assert!(Quality::NoSymbols.explanation().contains("fun_<address>"));
        assert!(
            Quality::DebugInfo.explanation().is_empty()
                && Quality::Missing.explanation().is_empty()
        );
        assert!(
            !Quality::NoSymbols.has_names()
                && !Quality::Missing.has_names()
                && Quality::FromDump.has_names()
        );
    }

    #[test]
    fn the_usage_summary_reads_naturally() {
        assert_eq!(
            usage(5, 5, 0, 4).summary(),
            "5 frames: 4 with source lines, 5 named"
        );
        assert_eq!(
            usage(1, 0, 1, 0).summary(),
            "1 frame: 0 with source lines, 0 named, 1 synthesized"
        );
    }
}
