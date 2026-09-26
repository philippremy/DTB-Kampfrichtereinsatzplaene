//! Cancellable background jobs that turn frames into [`syshints`] entries.
//!
//! A [`Job`] is shared between the worker thread and the crash dialog. The worker appends entries as it goes;
//! the dialog polls [`Job::progress`] for its spinner and, when the user skips or sends, calls
//! [`Job::cancel`] and takes [`Job::snapshot`] **immediately** — it never waits for the worker, and the snapshot
//! says truthfully whether it is complete.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use dtb_ke_crash::syshints::{Entry, FrameRef, Hints, Quality};

use crate::{cache, install_name, is_system_path};

#[derive(Default)]
struct State {
    entries: Vec<Entry>,
    /// System frames to ask about (set up front), examined so far, and how many of those got a name.
    total: u32,
    examined: u32,
    resolved: u32,
    done: bool,
    /// The whole thing could not run (`no-cache`, …).
    failure: Option<String>,
}

pub struct Job {
    cancel: AtomicBool,
    state: Mutex<State>,
}

impl Job {
    fn new() -> Arc<Self> {
        Arc::new(Self { cancel: AtomicBool::new(false), state: Mutex::new(State::default()) })
    }

    /// Ask the worker to stop at its next check. Whatever it found so far stays available.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// `(frames examined, frames to examine, finished)` — for the dialog's progress line.
    pub fn progress(&self) -> (u32, u32, bool) {
        let s = self.state.lock().unwrap();
        (s.examined, s.total, s.done)
    }

    pub fn is_done(&self) -> bool {
        self.state.lock().unwrap().done
    }

    /// The hints as they stand — safe to call at any moment, from any thread.
    pub fn snapshot(&self, producer: &str, quality: Quality, os_build: &str) -> Hints {
        let s = self.state.lock().unwrap();
        let (complete, reason) = match (&s.failure, s.done, self.is_cancelled()) {
            (Some(why), _, _) => (false, why.clone()),
            (None, true, _) => (true, String::new()),
            (None, false, true) => (false, "skipped".to_owned()),
            (None, false, false) => (false, "unfinished".to_owned()),
        };
        Hints {
            producer: producer.to_owned(),
            quality,
            complete,
            incomplete_reason: reason,
            frames_total: s.total,
            frames_resolved: s.resolved,
            os_build: os_build.to_owned(),
            entries: s.entries.clone(),
        }
    }

    fn add(&self, entry: Option<Entry>) {
        let mut s = self.state.lock().unwrap();
        s.examined += 1;
        if let Some(e) = entry {
            s.resolved += 1;
            if !s.entries.iter().any(|x| x.uuid == e.uuid && x.start == e.start) {
                s.entries.push(e);
            }
        }
    }

    fn finish(&self, failure: Option<String>) {
        let mut s = self.state.lock().unwrap();
        s.failure = failure;
        s.done = true;
    }

    fn set_total(&self, n: u32) {
        self.state.lock().unwrap().total = n;
    }
}

/// macOS: every OS frame looked up in the machine's dyld shared cache, local symbols included.
pub fn spawn_dyld_cache(frames: Vec<FrameRef>) -> Arc<Job> {
    let job = Job::new();
    let worker = job.clone();
    std::thread::Builder::new()
        .name("dtbke-syms-cache".into())
        .spawn(move || run_dyld_cache(&worker, &frames))
        .map_err(|e| job.finish(Some(format!("error: {e}"))))
        .ok();
    job
}

fn run_dyld_cache(job: &Job, frames: &[FrameRef]) {
    if !cache::host_available() {
        job.finish(Some("no-cache".into()));
        return;
    }
    let system: Vec<&FrameRef> = frames.iter().filter(|f| is_system_path(&f.path)).collect();
    job.set_total(system.len() as u32);

    // Each image's symbol table is built once (memoised in `cache`); cancelling takes effect between frames.
    for frame in system {
        if job.is_cancelled() {
            return;
        }
        let entry = cache::symbols(install_name(&frame.path), &frame.uuid).and_then(|symbols| {
            let (name, start, end) = symbols.lookup_range(frame.offset)?;
            Some(Entry { uuid: frame.uuid, start, end: end.max(frame.offset + 1), name: name.to_owned() })
        });
        job.add(entry);
    }
    job.finish(None);
}

/// iOS (and, for tests, macOS): `dladdr` on the images loaded in this process — exported symbols only.
#[cfg(any(target_os = "macos", target_os = "ios"))]
pub fn spawn_dladdr(frames: Vec<FrameRef>) -> Arc<Job> {
    let job = Job::new();
    let worker = job.clone();
    std::thread::Builder::new()
        .name("dtbke-syms-dladdr".into())
        .spawn(move || {
            let images = crate::approx::loaded_images();
            let system: Vec<&FrameRef> = frames.iter().filter(|f| is_system_path(&f.path)).collect();
            worker.set_total(system.len() as u32);
            for frame in system {
                if worker.is_cancelled() {
                    return;
                }
                worker.add(crate::approx::resolve(&images, frame));
            }
            worker.finish(None);
        })
        .map_err(|e| job.finish(Some(format!("error: {e}"))))
        .ok();
    job
}

#[cfg(test)]
mod tests {
    use super::*;
    use dtb_ke_crash::syshints::Quality;

    fn wait(job: &Job) {
        for _ in 0..600 {
            if job.is_done() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        panic!("job did not finish");
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    fn getpid_frame() -> FrameRef {
        let address = libc::getpid as *const () as usize;
        let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
        unsafe { libc::dladdr(address as *const libc::c_void, &mut info) };
        let header = info.dli_fbase as usize;
        let uuid = unsafe { crate::approx::image_uuid(header as *const u8) }.expect("uuid");
        FrameRef { thread: 1, uuid, offset: (address - header) as u64 + 2, path: "/usr/lib/system/libsystem_kernel.dylib".into() }
    }

    #[cfg(any(target_os = "macos", target_os = "ios"))]
    #[test]
    fn the_dladdr_job_completes_and_reports_an_approximation() {
        let job = spawn_dladdr(vec![
            getpid_frame(),
            // Not an OS library: never asked about, never counted.
            FrameRef { thread: 1, uuid: [1; 16], offset: 0x10, path: "/Users/x/App.app/App".into() },
        ]);
        wait(&job);
        let h = job.snapshot("ios-dladdr", Quality::Approximate, "TESTBUILD");
        assert!(h.complete, "{}", h.incomplete_reason);
        assert_eq!((h.frames_total, h.frames_resolved), (1, 1));
        assert_eq!(h.quality, Quality::Approximate);
        assert!(h.entries[0].name.ends_with("getpid"));
        assert_eq!(h.os_build, "TESTBUILD");
    }

    /// A frame in `CFRunLoopRun`, described the way a dump would: UUID + offset + the image's path.
    /// CoreFoundation exists only inside the dyld cache, so this exercises the cache reader for real.
    #[cfg(target_os = "macos")]
    fn core_foundation_frame() -> Option<FrameRef> {
        unsafe {
            let lib = libc::dlopen(c"/System/Library/Frameworks/CoreFoundation.framework/CoreFoundation".as_ptr(), libc::RTLD_NOW);
            if lib.is_null() {
                return None;
            }
            let address = libc::dlsym(lib, c"CFRunLoopRun".as_ptr()) as usize;
            let mut info: libc::Dl_info = std::mem::zeroed();
            if address == 0 || libc::dladdr(address as *const libc::c_void, &mut info) == 0 {
                return None;
            }
            let header = info.dli_fbase as usize;
            let path = std::ffi::CStr::from_ptr(info.dli_fname).to_string_lossy().into_owned();
            let uuid = crate::approx::image_uuid(header as *const u8)?;
            Some(FrameRef { thread: 1, uuid, offset: (address - header) as u64 + 8, path })
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn the_cache_job_names_frames_from_the_machines_own_cache() {
        let Some(real) = core_foundation_frame() else { return };
        if !cache::host_available() {
            return;
        }
        let unknown = FrameRef { uuid: [0x77; 16], ..real.clone() };
        let job = spawn_dyld_cache(vec![real.clone(), unknown]);
        wait(&job);
        let h = job.snapshot("macos-dyld-cache", Quality::Exact, "B");
        assert!(h.complete, "{}", h.incomplete_reason);
        assert_eq!((h.frames_total, h.frames_resolved), (2, 1), "the wrong-UUID frame stays unresolved");
        let e = &h.entries[0];
        assert!(e.name.contains("CFRunLoopRun"), "{}", e.name);
        assert!(e.start <= real.offset && real.offset < e.end);
    }

    /// The same frame through the iOS engine: an exported function, so `dladdr` names it too — the two engines
    /// agree where the exports are enough.
    #[cfg(target_os = "macos")]
    #[test]
    fn the_dladdr_engine_agrees_on_an_exported_function() {
        let Some(frame) = core_foundation_frame() else { return };
        let job = spawn_dladdr(vec![frame]);
        wait(&job);
        let h = job.snapshot("ios-dladdr", Quality::Approximate, "B");
        assert_eq!(h.frames_resolved, 1);
        assert!(h.entries[0].name.contains("CFRunLoopRun"), "{}", h.entries[0].name);
    }

    #[test]
    fn a_cancelled_job_says_skipped_and_keeps_what_it_had() {
        let job = Job::new();
        job.set_total(10);
        job.add(Some(Entry { uuid: [9; 16], start: 0, end: 4, name: "f".into() }));
        job.add(None);
        job.cancel();
        let h = job.snapshot("macos-dyld-cache", Quality::Exact, "B");
        assert!(!h.complete);
        assert_eq!(h.incomplete_reason, "skipped");
        assert_eq!((h.frames_total, h.frames_resolved, h.entries.len()), (10, 1, 1));
        assert_eq!(job.progress(), (2, 10, false));

    }

    #[test]
    fn a_failed_job_reports_why() {
        let job = Job::new();
        job.finish(Some("no-cache".into()));
        let h = job.snapshot("macos-dyld-cache", Quality::Exact, "B");
        assert!(!h.complete);
        assert_eq!(h.incomplete_reason, "no-cache");
    }
}

