//! Finding the system symbols of the OS a dump ran on, without asking: the dump records its OS build
//! (`26A428`, `23F77`, …), and Xcode / the simulator keep per-build material in well-known places.
//!
//! * **Xcode's `<platform> DeviceSupport/<version> (<build>)/…/Symbols`** — the system libraries Xcode
//!   extracted from a device (or another Mac) the first time it was connected. A plain tree of Mach-O images,
//!   indexed by UUID like any supplied folder. This is how real iOS devices are covered.
//! * **The simulator's dyld caches, `…/CoreSimulator/Caches/dyld/<host build>/<runtime>.<runtime build>/`** —
//!   where the runtime's shared cache lives once a simulator of that runtime has booted.
//!
//! Nothing here reads file contents; it only picks *which* folders are worth indexing, by name.

use std::path::{Path, PathBuf};

/// What the dump says about the OS it ran on.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemFacts {
    /// `"Mac OS X"`, `"iOS"`, `"Windows NT"`, …
    pub os: String,
    /// `"26.0"` (may be empty).
    pub version: String,
    /// `"26A428"` (may be empty; only Apple dumps record it).
    pub build: String,
}

impl std::fmt::Display for SystemFacts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.os)?;
        if !self.version.is_empty() {
            write!(f, " {}", self.version)?;
        }
        if !self.build.is_empty() {
            write!(f, " ({})", self.build)?;
        }
        Ok(())
    }
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Discovered {
    /// Folders of extracted system images, to index by UUID (`DirectorySource::add_root`).
    pub symbol_roots: Vec<PathBuf>,
    /// Directories that may hold dyld shared caches (`dyld::add_caches_from`).
    pub cache_dirs: Vec<PathBuf>,
}

/// Look in the standard places under the real `$HOME` and `/Library`.
pub fn discover(facts: &SystemFacts) -> Discovered {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    discover_in(facts, home.as_deref(), Path::new("/Library"))
}

pub fn discover_in(facts: &SystemFacts, home: Option<&Path>, library: &Path) -> Discovered {
    let mut found = Discovered::default();
    let build = facts.build.trim();
    if build.is_empty() {
        return found;
    }

    let subdirs = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.is_dir())
            .collect()
    };
    let name_of = |p: &Path| {
        p.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default()
    };

    // Xcode: "<Platform> DeviceSupport/<device?> <version> (<build>)/[<arch>/]Symbols"
    if let Some(home) = home {
        let xcode = home.join("Library/Developer/Xcode");
        for support in subdirs(&xcode)
            .into_iter()
            .filter(|p| name_of(p).ends_with("DeviceSupport"))
        {
            for entry in subdirs(&support)
                .into_iter()
                .filter(|p| name_of(p).contains(&format!("({build})")))
            {
                for candidate in std::iter::once(entry.clone()).chain(subdirs(&entry)) {
                    let symbols = candidate.join("Symbols");
                    if symbols.is_dir() {
                        found.symbol_roots.push(symbols);
                    }
                }
            }
        }
    }

    // Simulator caches: ".../CoreSimulator/Caches/dyld/<host build>/<...SimRuntime.iOS-26-5>.<runtime build>"
    let mut sim_roots = vec![library.join("Developer/CoreSimulator/Caches/dyld")];
    if let Some(home) = home {
        sim_roots.push(home.join("Library/Developer/CoreSimulator/Caches/dyld"));
    }
    for root in sim_roots {
        for host in subdirs(&root) {
            for runtime in subdirs(&host)
                .into_iter()
                .filter(|p| name_of(p).ends_with(&format!(".{build}")))
            {
                found.cache_dirs.push(runtime);
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("dtbke-discover-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn picks_only_the_folders_of_the_dumps_os_build() {
        let root = scratch("picks");
        let (home, library) = (root.join("home"), root.join("Library"));
        let xcode = home.join("Library/Developer/Xcode");
        for dir in [
            "iOS DeviceSupport/iPad14,1 26.0 (23A123)/Symbols/System",
            "iOS DeviceSupport/iPad14,1 26.1 (23B999)/Symbols/System",
            "macOS DeviceSupport/26.5.1 (25F80)/x86_64/Symbols/usr",
        ] {
            std::fs::create_dir_all(xcode.join(dir)).unwrap();
        }
        let sim = library.join("Developer/CoreSimulator/Caches/dyld/26A428");
        std::fs::create_dir_all(sim.join("com.apple.CoreSimulator.SimRuntime.iOS-26-5.23F77"))
            .unwrap();
        std::fs::create_dir_all(sim.join("com.apple.CoreSimulator.SimRuntime.iOS-26-4.23E11"))
            .unwrap();

        let facts = |b: &str| SystemFacts {
            os: "iOS".into(),
            version: "26.0".into(),
            build: b.into(),
        };

        let device = discover_in(&facts("23A123"), Some(&home), &library);
        assert_eq!(
            device.symbol_roots,
            vec![xcode.join("iOS DeviceSupport/iPad14,1 26.0 (23A123)/Symbols")]
        );
        assert!(device.cache_dirs.is_empty());

        let mac = discover_in(&facts("25F80"), Some(&home), &library);
        assert_eq!(
            mac.symbol_roots,
            vec![xcode.join("macOS DeviceSupport/26.5.1 (25F80)/x86_64/Symbols")]
        );

        let simulator = discover_in(&facts("23F77"), Some(&home), &library);
        assert_eq!(
            simulator.cache_dirs,
            vec![sim.join("com.apple.CoreSimulator.SimRuntime.iOS-26-5.23F77")]
        );

        assert_eq!(
            discover_in(&facts("99Z999"), Some(&home), &library),
            Discovered::default()
        );
        assert_eq!(
            discover_in(&facts(""), Some(&home), &library),
            Discovered::default()
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn facts_display_reads_naturally() {
        let f = SystemFacts {
            os: "iOS".into(),
            version: "26.5".into(),
            build: "23F77".into(),
        };
        assert_eq!(f.to_string(), "iOS 26.5 (23F77)");
        assert_eq!(
            SystemFacts {
                os: "Linux".into(),
                ..Default::default()
            }
            .to_string(),
            "Linux"
        );
    }
}
