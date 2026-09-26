//! System-symbol hints for a crash report: names for the frames in OS libraries, resolved **on the machine that
//! crashed** while the report dialog is up, and carried in the dump as a user stream.
//!
//! * macOS — [`dtb_ke_syms::spawn_dyld_cache`]: the machine's own dyld shared cache, local symbols included
//!   (**exact**).
//! * iOS — [`dtb_ke_syms::spawn_dladdr`]: exported symbols only (**approximate**); `dtb-ke-debugger` recognises
//!   that and asks for the device-support files to refine it.
//!
//! The user can skip the wait (or just send): the run is cancelled, whatever was found stays in the stream, and the
//! stream says it is incomplete. Nothing here blocks the dialog.

use std::path::PathBuf;
use std::sync::Arc;

use dtb_ke_crash::syshints::{self, FrameRef, Hints, Quality};
use dtb_ke_syms::Job;

pub struct HintsRun {
    job: Arc<Job>,
    frames: Vec<FrameRef>,
    os_build: String,
    producer: &'static str,
    quality: Quality,
    /// The dump as it was when the run started.
    dump: Vec<u8>,
    dump_path: Option<PathBuf>,
}

impl HintsRun {
    /// Start resolving `frames` in the background. `None` where there is nothing to do: an OS that has no engine
    /// (Windows / Linux resolve their system symbols from Microsoft's / the distro's servers instead), or no
    /// frames.
    pub fn start(frames: Vec<FrameRef>, os_build: String, dump: Vec<u8>, dump_path: Option<PathBuf>) -> Option<Arc<Self>> {
        if frames.is_empty() {
            return None;
        }
        #[cfg(target_os = "macos")]
        let (job, producer, quality) = (dtb_ke_syms::spawn_dyld_cache(frames.clone()), "macos-dyld-cache", Quality::Exact);
        #[cfg(target_os = "ios")]
        let (job, producer, quality) = (dtb_ke_syms::spawn_dladdr(frames.clone()), "ios-dladdr", Quality::Approximate);
        #[cfg(not(any(target_os = "macos", target_os = "ios")))]
        {
            let _ = (frames, os_build, dump, dump_path);
            return None;
        }
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        Some(Arc::new(Self { job, frames, os_build, producer, quality, dump, dump_path }))
    }

    pub fn job(&self) -> &Job {
        &self.job
    }

    /// The hints as they stand now (complete or not).
    pub fn snapshot(&self) -> Hints {
        self.job.snapshot(self.producer, self.quality, &self.os_build)
    }

    /// The dump with the hints stream appended. The original if patching fails — the dump is never lost to this.
    pub fn patched_dump(&self) -> Vec<u8> {
        let mut dmp = self.dump.clone();
        match syshints_patch(&mut dmp, &self.snapshot()) {
            Ok(()) => dmp,
            Err(err) => {
                log::warn!("cannot add the system-symbol hints to the dump: {err}");
                self.dump.clone()
            }
        }
    }

    /// Write the patched dump back over the file it came from (temp + rename), so the copy kept on disk carries
    /// the hints too. A no-op without a path or when patching changed nothing.
    pub fn persist(&self) {
        let Some(path) = &self.dump_path else { return };
        let patched = self.patched_dump();
        if patched == self.dump {
            return;
        }
        let tmp = path.with_extension("dmp.part");
        let result = std::fs::write(&tmp, &patched).and_then(|()| std::fs::rename(&tmp, path));
        if let Err(err) = result {
            log::warn!("cannot save the system-symbol hints into {}: {err}", path.display());
            let _ = std::fs::remove_file(&tmp);
        }
    }

    /// The hints as a section of the report's body — readable without the dump.
    pub fn body_section(&self) -> String {
        section(&self.frames, &self.snapshot())
    }
}

fn syshints_patch(dmp: &mut Vec<u8>, hints: &Hints) -> Result<(), dtb_ke_crash::patch::PatchError> {
    dtb_ke_crash::patch::append_stream(dmp, syshints::STREAM_TYPE, hints.encode().as_bytes())
}

/// `frames` named by `hints`, one line per resolved frame, under a heading that says how good the names are.
pub fn section(frames: &[FrameRef], hints: &Hints) -> String {
    let quality = match hints.quality {
        Quality::Exact => "exact",
        Quality::Approximate => "approximate — exported symbols only",
    };
    let state = if hints.complete { "complete".to_owned() } else { format!("incomplete: {}", hints.incomplete_reason) };
    let mut out = format!(
        "\nSystem frames, named on the crashing machine ({quality}; {state}; {} of {} frames):\n",
        hints.frames_resolved, hints.frames_total
    );
    for f in frames {
        if let Some(e) = hints.lookup(&f.uuid, f.offset) {
            out.push_str(&format!("  [thread {:#x}] {} +{:#x}\n", f.thread, e.name, f.offset - e.start));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtb_ke_crash::syshints::Entry;

    #[test]
    fn the_section_names_frames_and_says_how_good_and_how_complete() {
        let uuid = [3; 16];
        let frames = vec![
            FrameRef { thread: 0x103, uuid, offset: 0x1010, path: "/usr/lib/x.dylib".into() },
            FrameRef { thread: 0x103, uuid, offset: 0x9000, path: "/usr/lib/x.dylib".into() },
        ];
        let hints = Hints {
            producer: "ios-dladdr".into(),
            quality: Quality::Approximate,
            complete: false,
            incomplete_reason: "skipped".into(),
            frames_total: 2,
            frames_resolved: 1,
            os_build: "23F77".into(),
            entries: vec![Entry { uuid, start: 0x1000, end: 0x1011, name: "dispatch_x".into() }],
        };
        let text = section(&frames, &hints);
        assert!(text.contains("approximate"), "{text}");
        assert!(text.contains("incomplete: skipped"), "{text}");
        assert!(text.contains("1 of 2 frames"), "{text}");
        assert!(text.contains("[thread 0x103] dispatch_x +0x10"), "{text}");
        assert!(!text.contains("0x9000"), "unresolved frames are not listed");
    }
}
