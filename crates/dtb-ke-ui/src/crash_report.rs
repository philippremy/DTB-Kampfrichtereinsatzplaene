//! The crash reporter — a second, minimal mode of this same binary.
//!
//! When the app faults, the out-of-process crash helper (`dtb-ke-crash`) writes
//! a minidump, then **relaunches this executable** and streams us, on stdin, a
//! small digest followed by the raw dump —
//! `reason \0 address \0 crashing_tid \0 panic_msg \0 stack \0 <minidump bytes>`.
//! We recognise that we are the reporter by our *parent process*: the helper
//! (`getppid()` → a binary named [`dtb_ke_crash::HELPER_FILE_NAME`]). No CLI
//! flag, no environment variable. "Bericht senden" mails the digest + stack (and
//! optionally the `.dtbkedmp`) via [`crate::mail`]; the `.dtbkedmp` is attached only if the
//! user ticks the box.
//!
//! [`main`](crate) branches here **before** logging, settings, and — crucially —
//! the crash-handler install, so a fault in the reporter can never spawn another
//! reporter. The process exits with a [`dtb_ke_crash::report`] code that is the
//! helper's verdict: only `SENT` lets it delete the dump.

#![cfg_attr(
    not(any(target_os = "macos", target_os = "windows", target_os = "linux")),
    allow(dead_code)
)]

use gpui_kit::Subscription;
use gpui_kit::{
    App, AppContext, Bounds, Context, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, Render, ScrollHandle, Size, StatefulInteractiveElement, Styled,
    SharedString, TitlebarOptions, Window, WindowBounds, WindowKind, WindowOptions, div, prelude::FluentBuilder,
    px,
};
use gpui_kit::base::Scrollbar;

use dtb_ke_crash::report;

use crate::build_info;
use crate::crash_hints::HintsRun;
use crate::i18n::ActiveLocale;
use crate::components::checkbox::Checkbox;
use crate::components::{Button, ButtonTone, Icon, Spinner};
use crate::mail;
use crate::theme::{ActiveTheme, Appearance, Theme, ThemeMode};

/// Are we a crash-reporter relaunch? True iff our parent process is the crash
/// helper (a binary called [`dtb_ke_crash::HELPER_FILE_NAME`]).
#[cfg(target_os = "macos")]
pub fn launched_by_crash_helper() -> bool {
    let ppid = unsafe { libc::getppid() };
    if ppid <= 1 {
        return false;
    }
    let mut buf = [0u8; 4096]; // PROC_PIDPATHINFO_MAXSIZE
    let n = unsafe {
        libc::proc_pidpath(
            ppid,
            buf.as_mut_ptr() as *mut libc::c_void,
            buf.len() as u32,
        )
    };
    if n <= 0 {
        return false;
    }
    let path = String::from_utf8_lossy(&buf[..n as usize]);
    let base = path.rsplit('/').next().unwrap_or("");
    base.eq_ignore_ascii_case(dtb_ke_crash::HELPER_FILE_NAME)
        || base.eq_ignore_ascii_case("dtb-ke-crashhandler")
}

/// Windows: our parent process (Toolhelp snapshot → `th32ParentProcessID` → that
/// entry's `szExeFile`) is a binary named [`dtb_ke_crash::HELPER_FILE_NAME`].
#[cfg(target_os = "windows")]
pub fn launched_by_crash_helper() -> bool {
    type Handle = *mut core::ffi::c_void;
    const INVALID_HANDLE_VALUE: Handle = usize::MAX as Handle;
    const TH32CS_SNAPPROCESS: u32 = 0x0000_0002;

    #[repr(C)]
    struct ProcessEntry32W {
        dw_size: u32,
        cnt_usage: u32,
        th32_process_id: u32,
        th32_default_heap_id: usize,
        th32_module_id: u32,
        cnt_threads: u32,
        th32_parent_process_id: u32,
        pc_pri_class_base: i32,
        dw_flags: u32,
        sz_exe_file: [u16; 260],
    }

    unsafe extern "system" {
        fn CreateToolhelp32Snapshot(flags: u32, pid: u32) -> Handle;
        fn Process32FirstW(snap: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn Process32NextW(snap: Handle, entry: *mut ProcessEntry32W) -> i32;
        fn CloseHandle(h: Handle) -> i32;
        fn GetCurrentProcessId() -> u32;
    }

    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let me = GetCurrentProcessId();
        let mut entry: ProcessEntry32W = core::mem::zeroed();
        entry.dw_size = size_of::<ProcessEntry32W>() as u32;

        // pass 1: our parent pid
        let mut ppid = 0u32;
        if Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == me {
                    ppid = entry.th32_parent_process_id;
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }

        // pass 2: the parent's exe name
        let mut hit = false;
        if ppid != 0 && Process32FirstW(snap, &mut entry) != 0 {
            loop {
                if entry.th32_process_id == ppid {
                    let n = entry
                        .sz_exe_file
                        .iter()
                        .position(|&c| c == 0)
                        .unwrap_or(entry.sz_exe_file.len());
                    let name = String::from_utf16_lossy(&entry.sz_exe_file[..n]);
                    hit = name.eq_ignore_ascii_case(dtb_ke_crash::HELPER_FILE_NAME);
                    break;
                }
                if Process32NextW(snap, &mut entry) == 0 {
                    break;
                }
            }
        }
        CloseHandle(snap);
        hit
    }
}

/// Linux: our parent process (`/proc/self/status` → `PPid`) is a binary whose
/// `/proc/<ppid>/exe` basename is [`dtb_ke_crash::HELPER_FILE_NAME`].
#[cfg(target_os = "linux")]
pub fn launched_by_crash_helper() -> bool {
    let Some(ppid) = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|status| {
            status.lines().find_map(|line| {
                line.strip_prefix("PPid:")
                    .and_then(|v| v.trim().parse::<i32>().ok())
            })
        })
    else {
        return false;
    };
    if ppid <= 1 {
        return false;
    }

    let exe_matches = std::fs::read_link(format!("/proc/{ppid}/exe"))
        .ok()
        .as_deref()
        .and_then(|p| p.file_name())
        .map(|name| {
            name.to_string_lossy()
                .eq_ignore_ascii_case(dtb_ke_crash::HELPER_FILE_NAME)
        })
        .unwrap_or(false);
    if exe_matches {
        return true;
    }

    // Fallback: `/proc/<ppid>/comm` (truncated to 15 chars by the kernel).
    std::fs::read_to_string(format!("/proc/{ppid}/comm"))
        .map(|comm| {
            let comm = comm.trim();
            !comm.is_empty()
                && dtb_ke_crash::HELPER_FILE_NAME
                    .as_bytes()
                    .starts_with(comm.as_bytes())
        })
        .unwrap_or(false)
}

#[cfg(not(any(target_os = "macos", target_os = "windows", target_os = "linux")))]
pub fn launched_by_crash_helper() -> bool {
    false
}

/// Read the digest from stdin, show the dialog, and return the `report::*` exit
/// code. Blocks until the user answers (or closes the window).
pub fn run() -> i32 {
    let Some(digest) = read_digest_from_stdin() else {
        return report::ERROR;
    };

    // Debug hook: print the composed report body (no GUI, no send) so the
    // transmission path can be inspected without a live SMTP server.
    if std::env::var_os("DTB_KE_CRASH_REPORT_DUMP").is_some() {
        let (subject, body) = compose(&digest);
        eprintln!(
            "--- REPORT ---\nSubject: {subject}\n\n{body}\n--- dmp: {} B ({}) · frames: {} · os build: {:?} · log: {} B · mail::available = {} ---",
            digest.minidump.len(),
            digest.dump_path,
            digest.frames.len(),
            digest.os_build,
            newest_log().len(),
            mail::available()
        );
    }

    // Test hook (set on the crashed app; propagates down the relaunch chain):
    //   sent|queued|declined       — answer without a GUI (headless verdict)
    //   gui-queued|gui-declined    — show the GUI, then auto-answer after 800 ms
    //   gui-autosend               — show the GUI, then auto-click "Bericht senden"
    //                                (exercises the spinner → "Gesendet!" flow;
    //                                pair with DTB_KE_MAIL_FAKE=ok|err)
    let mut autosend = false;
    if let Ok(v) = std::env::var("DTB_KE_CRASH_REPORT_TEST") {
        match v.as_str() {
            "hints" => {} // handled above (before this match would be reached)
            "sent" => return report::SENT,
            "queued" => return report::QUEUED,
            "declined" => return report::DECLINED,
            "gui-autosend" => autosend = true,
            "gui-queued" | "gui-declined" => {
                let code = if v.ends_with("queued") {
                    report::QUEUED
                } else {
                    report::DECLINED
                };
                std::thread::spawn(move || {
                    std::thread::sleep(std::time::Duration::from_millis(800));
                    std::process::exit(code);
                });
            }
            _ => return report::DECLINED,
        }
    }

    let hints = start_hints(&digest);

    // Test hook: `DTB_KE_CRASH_REPORT_TEST=hints` resolves the system symbols, saves them into the dump and
    // exits — the whole pipeline without a GUI. (Set on the crashed app, like the other test hooks.)
    if std::env::var("DTB_KE_CRASH_REPORT_TEST").as_deref() == Ok("hints") {
        if let Some(h) = &hints {
            let start = std::time::Instant::now();
            while !h.job().is_done() && start.elapsed() < std::time::Duration::from_secs(120) {
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
        finish_hints(&hints, report::DECLINED);
        return report::DECLINED;
    }

    let summary = Summary::build(&digest);
    let (subject, body) = compose(&digest);
    let subject = crate::debug::crash::mark_subject(subject);
    let dmp = digest.minidump;
    let log = newest_log();

    // The verdict is delivered by `std::process::exit(report::*)` straight from
    // the button / window-close handlers — **not** by returning it here.
    // `cx.quit()` on macOS is `[NSApp terminate:]`, which calls `exit(0)`
    // internally, so anything after `application().run()` (and any exit code we'd
    // return) is unreachable — and `exit(0)` would read as `SENT`, silently
    // discarding every dump.
    gpui_kit::platform::application().run(move |cx: &mut App| {
        // macOS: don't show a second dock icon for the reporter.
        #[cfg(target_os = "macos")]
        {
            use objc2::MainThreadMarker;
            use objc2_app_kit::{NSApp, NSApplicationActivationPolicy};
            let mtm = MainThreadMarker::new()
                .expect("on_finish_launching always runs on the Main Thread");
            NSApp(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        gpui_kit::base::init(cx);
        // The reporter is its own process: load the user's saved settings (for their language choice — not `Settings::init`,
        // whose developer-option hooks have no business here) and install the locale.
        cx.set_global(crate::settings::Settings::load());
        crate::i18n::Locale::install(cx);
        Theme::install(
            ThemeMode::System,
            Appearance::from(cx.window_appearance()),
            cx,
        );
        // Closing the window == declining.
        let on_close_hints = hints.clone();
        cx.on_window_closed(move |_, _| {
            finish_hints(&on_close_hints, report::DECLINED);
            std::process::exit(report::DECLINED)
        })
        .detach();

        let opts = window_options(cx);
        let opened = cx.open_window(opts, move |window, cx| {
            cx.new(|cx| {
                ReportWindow::new(
                    summary,
                    subject,
                    body,
                    dmp,
                    log,
                    autosend,
                    exit_verdict(hints.clone()),
                    hints.clone(),
                    false,
                    window,
                    cx,
                )
            })
        });
        if opened.is_err() {
            std::process::exit(report::ERROR);
        }
        cx.activate(true);
    });

    // `application().run()` only returns if it never entered the loop.
    report::DECLINED
}

/// The crashed process's session log — the newest `*.log` in the log directory.
/// The reporter never registers a logger of its own, so nothing newer exists.
/// Capped at 4 MB (the tail, on a line boundary); empty if none is found.
fn newest_log() -> Vec<u8> {
    newest_log_before(None)
}

/// The newest `*.log`, optionally only among files last written at or before `cutoff` — the iOS
/// reporter runs in the *next* launch, whose own (newer) log must not be mistaken for the crashed one.
fn newest_log_before(cutoff: Option<std::time::SystemTime>) -> Vec<u8> {
    const CAP: usize = 1 << 22;
    let Ok(dir) = std::fs::read_dir(crate::filesystem::FilesystemHelper::instance().get_log_dir())
    else {
        return Vec::new();
    };
    let newest = dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .filter(|(mtime, _)| cutoff.is_none_or(|c| *mtime <= c))
        .max_by_key(|(mtime, _)| *mtime)
        .map(|(_, path)| path);
    let Some(path) = newest else {
        return Vec::new();
    };
    let Ok(bytes) = std::fs::read(&path) else {
        return Vec::new();
    };
    if bytes.len() <= CAP {
        return bytes;
    }
    let start = bytes.len() - CAP;
    let start = bytes[start..]
        .iter()
        .position(|&b| b == b'\n')
        .map(|nl| start + nl + 1)
        .unwrap_or(start);
    let mut tail = format!(
        "… (gekürzt, {} von {} Bytes)\n",
        bytes.len() - start,
        bytes.len()
    )
    .into_bytes();
    tail.extend_from_slice(&bytes[start..]);
    tail
}

/// The helper's digest — `reason` `\0` `address` `\0` `crashing_tid` `\0`
/// `panic_msg` `\0` `stack` `\0` `frames` `\0` `dump_path` `\0` `os_build` `\0` `<minidump bytes>`.
struct Digest {
    /// Human-readable crash reason from the minidump (`minidump::CrashReason`
    /// Display), e.g. `EXC_BAD_ACCESS / KERN_INVALID_ADDRESS`. Empty if unknown.
    reason: String,
    /// Faulting address, `0x…`, or empty.
    address: String,
    /// Crashing thread — `«name» (0x…)` or `0x…`, or empty.
    crashing_tid: String,
    /// The Rust panic message (`PanicHookInfo` Display), or empty.
    panic_msg: String,
    /// Symbol-free stack walk of the crashing thread + the module table (no
    /// process memory) — goes into the report body.
    stack: String,
    /// Every thread's frames as (image UUID, offset) — what the system-symbol hints are computed from.
    frames: Vec<dtb_ke_crash::syshints::FrameRef>,
    /// Where the helper kept the dump (the reporter rewrites it with the hints), or empty.
    dump_path: String,
    /// The OS build the dump ran on (`26A428`), or empty.
    os_build: String,
    /// The raw minidump — attached to the report only if the user opts in.
    minidump: Vec<u8>,
}

fn read_digest_from_stdin() -> Option<Digest> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).ok()?;

    let mut fields = buf.splitn(9, |&b| b == 0);
    let mut next = || {
        fields
            .next()
            .map(|f| String::from_utf8_lossy(f).into_owned())
    };
    let reason = next()?;
    let address = next()?;
    let crashing_tid = next()?;
    let panic_msg = next()?;
    let stack = next()?;
    let frames = dtb_ke_crash::syshints::parse_frames(&next()?);
    let dump_path = next()?;
    let os_build = next()?;
    let minidump = fields.next().map(<[u8]>::to_vec).unwrap_or_default();
    Some(Digest {
        reason,
        address,
        crashing_tid,
        panic_msg,
        stack,
        frames,
        dump_path,
        os_build,
        minidump,
    })
}

fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(500.), px(500.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(cx.t("crash-report.window-title")),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    }
}

// ── the pre-digested view of a dump ──────────────────────────────────────

/// What the dialog's headline says — translated when rendered (the summary is built before a locale exists).
#[derive(Clone)]
enum Kind {
    Panic,
    Crash,
    /// The crash reason as the OS words it (`EXC_BAD_ACCESS / KERN_INVALID_ADDRESS`).
    Reason(String),
}

/// The labels of the detail rows — translated when rendered.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Row {
    Location,
    Signal,
    Address,
    Thread,
    Time,
}

impl Row {
    fn key(self) -> &'static str {
        match self {
            Row::Location => "crash-report.row-location",
            Row::Signal => "crash-report.row-signal",
            Row::Address => "crash-report.row-address",
            Row::Thread => "crash-report.row-thread",
            Row::Time => "crash-report.row-time",
        }
    }
}

#[derive(Clone)]
struct Summary {
    kind: Kind,
    message: Option<String>,
    rows: Vec<(Row, String)>,
    /// When the report was shown; formatted with the locale's own date format at render time.
    at: chrono::DateTime<chrono::Local>,
}

impl Summary {
    fn build(d: &Digest) -> Self {
        let is_panic = !d.panic_msg.trim().is_empty();
        let kind = if is_panic {
            Kind::Panic
        } else if d.reason.is_empty() {
            Kind::Crash
        } else {
            Kind::Reason(d.reason.clone())
        };

        let mut rows: Vec<(Row, String)> = Vec::new();

        // The panic hook records `PanicHookInfo`'s Display:
        // "panicked at <file>:<line>:<col>:\n<payload>". Show the payload as the
        // message and lift the location into its own row.
        let message = {
            let m = d.panic_msg.trim();
            if m.is_empty() {
                None
            } else {
                let (header, payload) = m.split_once('\n').unwrap_or(("", m));
                let loc = header
                    .trim()
                    .strip_prefix("panicked at ")
                    .map(|l| l.trim_end_matches(':'))
                    .filter(|l| !l.is_empty());
                if let Some(loc) = loc {
                    rows.push((Row::Location, loc.to_string()));
                }
                let payload = payload.trim().replace('\n', " ");
                Some(truncate(&payload, 240))
            }
        };

        // Show the technical reason for a panic too (as a row, since `kind` is
        // the friendlier label there).
        if is_panic && !d.reason.is_empty() {
            rows.push((Row::Signal, d.reason.clone()));
        }
        if !d.address.is_empty() && d.address != "0x0000000000000000" {
            rows.push((Row::Address, d.address.clone()));
        }
        if !d.crashing_tid.is_empty() {
            rows.push((Row::Thread, d.crashing_tid.clone()));
        }

        Self { kind, message, rows, at: chrono::Local::now() }
    }
}

/// The e-mail subject + plain-text body for a report. The body carries only the
/// digest + the symbol-free stack (no process memory) — the raw `.dtbkedmp` is a
/// separate, opt-in attachment.
fn compose(d: &Digest) -> (String, String) {
    let is_panic = !d.panic_msg.trim().is_empty();
    let kind = if is_panic {
        "Panic"
    } else if d.reason.is_empty() {
        "Exception"
    } else {
        d.reason.split(['/', ' ']).next().unwrap_or("Absturz")
    };
    let subject = format!(
        "Crash Report · {kind} · DTB Kampfrichtereinsatzpläne v{}",
        build_info::APP_VERSION
    );

    let mut body = String::new();
    body.push_str(&format!(
        "DTB Kampfrichtereinsatzpläne {} ({})\n",
        build_info::APP_VERSION,
        build_info::PROFILE
    ));
    body.push_str(&format!(
        "Target: {} · {}\n",
        build_info::TARGET_TRIPLE,
        chrono::Local::now().format("%Y-%m-%d %H:%M:%S %:z")
    ));
    body.push_str(&format!("\nReason:   {}\n", opt(&d.reason)));
    body.push_str(&format!("Address: {}\n", opt(&d.address)));
    body.push_str(&format!("Thread:  {}\n", opt(&d.crashing_tid)));
    if is_panic {
        body.push_str(&format!("\nPanic:\n{}\n", d.panic_msg.trim()));
    }
    if !d.stack.trim().is_empty() {
        body.push('\n');
        body.push_str(d.stack.trim_end());
        body.push('\n');
    }
    (subject, body)
}

fn opt(s: &str) -> &str {
    if s.trim().is_empty() { "—" } else { s }
}

// ── the window ───────────────────────────────────────────────────────────

#[derive(Clone)]
enum SendState {
    Idle,
    Sending,
    /// Delivered — "Gesendet!" is shown, then the window closes.
    Sent,
    Failed(String),
}

impl SendState {
    /// Whether the checkboxes / buttons should be inert.
    fn busy(&self) -> bool {
        matches!(self, SendState::Sending | SendState::Sent)
    }
}

/// How a verdict (`report::*`) leaves the reporter: the desktop reporter is its own process and exits
/// with it (the helper acts on the code); on iOS it is a sheet in the running app and just closes.
type Verdict = std::rc::Rc<dyn Fn(i32, &mut App)>;

fn exit_verdict(hints: Option<std::sync::Arc<HintsRun>>) -> Verdict {
    std::rc::Rc::new(move |code, _| {
        finish_hints(&hints, code);
        std::process::exit(code)
    })
}

/// Begin resolving the system-library frames of `digest` (macOS / iOS only).
fn start_hints(digest: &Digest) -> Option<std::sync::Arc<HintsRun>> {
    let path = (!digest.dump_path.is_empty()).then(|| std::path::PathBuf::from(&digest.dump_path));
    HintsRun::start(digest.frames.clone(), digest.os_build.clone(), digest.minidump.clone(), path)
}

/// The report is being closed with `code`: stop any running resolution and, unless the report was delivered (the
/// dump is deleted then), save what was resolved into the dump kept on disk.
fn finish_hints(hints: &Option<std::sync::Arc<HintsRun>>, code: i32) {
    let Some(h) = hints else { return };
    h.job().cancel();
    if code != report::SENT {
        h.persist();
    }
}

struct ReportWindow {
    verdict: Verdict,
    /// The sample report: "sending" never touches SMTP.
    simulate_send: bool,
    summary: Summary,
    subject: String,
    body: String,
    dmp: Vec<u8>,
    log: Vec<u8>,
    /// "Speicherabbild anhängen" — **off** by default.
    attach_dump: bool,
    /// "Protokoll anhängen" — **off** by default (only offered if `log` is
    /// non-empty).
    attach_log: bool,
    state: SendState,
    /// The system-symbol run for this crash, if the platform has one (macOS: the dyld cache; iOS: `dladdr`).
    hints: Option<std::sync::Arc<HintsRun>>,
    /// The detail card's own scroll position (kind/message/rows) — a long
    /// value (e.g. the panic location, a deep path inside a dependency
    /// rather than a short local one) or a long message can overflow the
    /// card in either direction, so it scrolls instead of just running over.
    scroll: ScrollHandle,
    _appearance_sub: Subscription,
}

impl ReportWindow {
    #[allow(clippy::too_many_arguments)]
    fn new(
        summary: Summary,
        subject: String,
        body: String,
        dmp: Vec<u8>,
        log: Vec<u8>,
        autosend: bool,
        verdict: Verdict,
        hints: Option<std::sync::Arc<HintsRun>>,
        simulate_send: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        // Startup value + follow OS light/dark changes.
        Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        let appearance_sub = cx.observe_window_appearance(window, |_, window, cx| {
            Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        });

        // `DTB_KE_CRASH_REPORT_TEST=gui-autosend`: click "Bericht senden" for the
        // user once the window is up, so the spinner → "Gesendet!" flow can be
        // exercised headlessly (pair with `DTB_KE_MAIL_FAKE=ok|err`).
        if autosend {
            cx.spawn(async move |this, cx| {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(300))
                    .await;
                this.update(cx, |this, cx| this.send(cx)).ok();
            })
            .detach();
        }

        // Repaint while the system symbols resolve, so the progress line moves; stops once the run ends or is skipped.
        if let Some(h) = hints.clone() {
            cx.spawn(async move |this, cx| {
                loop {
                    cx.background_executor().timer(std::time::Duration::from_millis(150)).await;
                    let over = h.job().is_done() || h.job().is_cancelled();
                    if this.update(cx, |_, cx| cx.notify()).is_err() || over {
                        break;
                    }
                }
            })
            .detach();
        }

        Self {
            verdict,
            hints,
            simulate_send,
            summary,
            subject,
            body,
            dmp,
            log,
            attach_dump: false,
            attach_log: false,
            state: SendState::Idle,
            scroll: ScrollHandle::new(),
            _appearance_sub: appearance_sub,
        }
    }

    /// Send the report on the background executor; on success show "Gesendet!"
    /// for 5 s then exit `SENT`, on failure surface the error and stay open.
    fn send(&mut self, cx: &mut Context<Self>) {
        if self.state.busy() {
            return;
        }
        self.state = SendState::Sending;
        cx.notify();

        // Sending does not wait for the system symbols: stop the run and send what it has. The stream in the dump
        // (and the section in the body) say if that is incomplete.
        let mut body = self.body.clone();
        let mut dmp = self.dmp.clone();
        if let Some(h) = &self.hints {
            h.job().cancel();
            body.push_str(&h.body_section());
            dmp = h.patched_dump();
        }

        let mut attachments = Vec::new();
        if self.attach_dump {
            attachments.push((
                format!("crash.{}", dtb_ke_crash::DUMP_EXTENSION),
                "application/octet-stream",
                dmp,
            ));
        }
        if self.attach_log {
            attachments.push((
                "session.log".to_owned(),
                "text/plain; charset=utf-8",
                self.log.clone(),
            ));
        }
        let report = mail::Report {
            subject: self.subject.clone(),
            body,
            attachments,
        };
        let verdict = self.verdict.clone();
        let simulate = self.simulate_send;
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    if simulate { mail::send_simulated() } else { mail::send(report) }
                })
                .await;
            match result {
                Ok(()) => {
                    this.update(cx, |this, cx| {
                        this.state = SendState::Sent;
                        cx.notify();
                    })
                    .ok();
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(3))
                        .await;
                    cx.update(|cx| verdict(report::SENT, cx));
                }
                Err(err) => {
                    this.update(cx, |this, cx| {
                        this.state = SendState::Failed(err.to_string());
                        cx.notify();
                    })
                    .ok();
                }
            }
        })
        .detach();
    }
}

impl Render for ReportWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_lg_px();
        let s = &self.summary;

        div()
            .size_full()
            .bg(c.background)
            .text_color(c.foreground)
            .flex()
            .flex_col()
            .p(px(20.))
            .when_else(
                cfg!(target_os = "macos"),
                |el| el.pt(px(40.)),
                |el| el.pt(px(20.)),
            )
            .gap(px(12.))
            .child(
                div()
                    .text_size(px(15.))
                    .font_weight(FontWeight::BOLD)
                    .child(cx.t("crash-report.title")),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(cx.t("crash-report.intro")),
            )
            .child(
                // The outer frame (border/bg/rounding) stays a fixed box;
                // the actual kind/message/row content lives in a scrollable
                // inner div below, since a long row value (a deep path
                // inside a dependency rather than a short local one, say)
                // or a long message can overflow the frame in either
                // direction — it scrolls instead of just running over it.
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .p(px(12.))
                    .bg(c.surface)
                    .border_1()
                    .border_color(c.border)
                    .rounded(radius)
                    .child(
                        div()
                            .id("crash-detail-scroll")
                            .size_full()
                            .flex()
                            .flex_col()
                            .gap(px(6.))
                            .overflow_x_scroll()
                            .overflow_y_scroll()
                            .track_scroll(&self.scroll)
                            .child(
                                div()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(13.))
                                    .child(match &s.kind {
                                        Kind::Panic => cx.t("crash-report.kind-panic").to_string(),
                                        Kind::Crash => cx.t("crash-report.kind-crash").to_string(),
                                        Kind::Reason(r) => r.clone(),
                                    }),
                            )
                            .when_some(s.message.clone(), |el, m| {
                                el.child(
                                    div().text_size(px(12.)).text_color(c.foreground).child(m),
                                )
                            })
                            .children(
                                s.rows
                                    .iter()
                                    .map(|(k, v)| (cx.t(k.key()).to_string(), v.clone()))
                                    .chain(std::iter::once((
                                        cx.t(Row::Time.key()).to_string(),
                                        s.at.format(&cx.t("crash-report.time-format")).to_string(),
                                    )))
                                    .map(|(k, v)| detail_row(k, v, c.muted_foreground)),
                            ),
                    )
                    .child(Scrollbar::new(&self.scroll)),
            )
            .child(self.footer(cx))
    }
}

impl ReportWindow {
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let verdict = self.verdict.clone();
        let c = cx.theme().color;
        let weak = cx.entity().downgrade();

        // Status of the system-symbol run: a spinner with a way to skip it while it works; afterwards the *result* stays
        // (it often finishes too fast to see the spinner, and a failure should not vanish).
        let hints_row = self.hints.as_ref().map(|h| {
            let (examined, total, finished) = h.job().progress();
            let cancelled = h.job().is_cancelled();
            let snap = h.snapshot();
            let counts = [
                ("resolved", snap.frames_resolved.to_string()),
                ("total", snap.frames_total.to_string()),
            ];
            let counts: Vec<(&str, &str)> = counts.iter().map(|(k, v)| (*k, v.as_str())).collect();
            let skip_weak = weak.clone();
            if !finished && !cancelled {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Spinner::new().size(px(13.)))
                    .child(
                        div()
                            .flex_1()
                            .text_size(px(11.))
                            .text_color(c.muted_foreground)
                            .child(cx.t_fmt(
                                "crash-report.hints-running",
                                &[("done", &examined.to_string()), ("total", &total.to_string())],
                            )),
                    )
                    .child(Button::new("cr-skip-hints", cx.t("crash-report.hints-skip")).tone(ButtonTone::Ghost).on_click(
                        move |_, _, cx| {
                            skip_weak
                                .update(cx, |this, cx| {
                                    if let Some(h) = &this.hints {
                                        h.job().cancel();
                                    }
                                    cx.notify();
                                })
                                .ok();
                        },
                    ))
                    .into_any_element()
            } else {
                let (text, color) = if finished && snap.complete {
                    (cx.t_fmt("crash-report.hints-done", &counts), c.muted_foreground)
                } else if finished && snap.incomplete_reason == "no-cache" {
                    (cx.t("crash-report.hints-failed-no-cache").to_string(), c.warn)
                } else if finished {
                    (cx.t_fmt("crash-report.hints-failed-error", &[("reason", &snap.incomplete_reason)]), c.warn)
                } else {
                    (cx.t_fmt("crash-report.hints-skipped", &counts), c.muted_foreground)
                };
                div().text_size(px(11.)).text_color(color).child(text).into_any_element()
            }
        });

        // Transport not compiled in → nothing to do but close.
        if !mail::available() && !self.simulate_send {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .children(hints_row)
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.muted_foreground)
                        .child(cx.t("crash-report.no-transport")),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("cr-close", cx.t("crash-report.close"))
                            .on_click(move |_, _, cx| verdict(report::DECLINED, cx)),
                    ),
                );
        }

        // Delivered → a brief "Gesendet!" before the window auto-closes.
        if matches!(self.state, SendState::Sent) {
            return div().flex().items_center().gap(px(8.)).child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(13.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(c.ok)
                    .child(Icon::Check.size(px(15.)).color(c.ok))
                    .child(cx.t("crash-report.sent-thanks")),
            );
        }

        let busy = self.state.busy();
        let failed = matches!(self.state, SendState::Failed(_));

        let dump_row = self.attach_row(
            "cr-attach-dump",
            cx.t("crash-report.attach-dump"),
            self.attach_dump,
            Attach::Dump,
            busy,
            &weak,
            c.muted_foreground,
        );
        let log_row = (!self.log.is_empty()).then(|| {
            self.attach_row(
                "cr-attach-log",
                cx.t("crash-report.attach-log"),
                self.attach_log,
                Attach::Log,
                busy,
                &weak,
                c.muted_foreground,
            )
        });

        let send_weak = weak.clone();
        let send_btn = Button::new(
            "cr-send",
            if failed { cx.t("crash-report.retry") } else { cx.t("crash-report.send") },
        )
        .tone(ButtonTone::Primary)
        .disabled(busy)
        .on_click(move |_, _, cx| {
            send_weak.update(cx, |this, cx| this.send(cx)).ok();
        });

        let left_btn = if failed {
            Button::new("cr-keep", cx.t("crash-report.keep-local"))
                .on_click(move |_, _, cx| verdict(report::QUEUED, cx))
        } else {
            Button::new("cr-decline", cx.t("crash-report.decline"))
                .disabled(busy)
                .on_click(move |_, _, cx| verdict(report::DECLINED, cx))
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
            .children(hints_row)
            .child(dump_row)
            .children(log_row)
            .when(failed, |el| {
                let SendState::Failed(msg) = &self.state else {
                    return el;
                };
                el.child(
                    div()
                        .mt(px(2.))
                        .text_size(px(11.))
                        .text_color(c.critical)
                        .child(msg.clone()),
                )
            })
            .child(
                div()
                    .mt(px(2.))
                    .flex()
                    .items_center()
                    .justify_end()
                    .gap(px(8.))
                    .when(busy, |el| {
                        el.child(Spinner::new().size(px(13.))).child(
                            div()
                                .flex_1()
                                .text_size(px(11.))
                                .text_color(c.muted_foreground)
                                .child(cx.t("crash-report.sending")),
                        )
                    })
                    .child(left_btn)
                    .child(send_btn),
            )
    }

    #[allow(clippy::too_many_arguments)]
    fn attach_row(
        &self,
        id: &'static str,
        label: SharedString,
        checked: bool,
        which: Attach,
        busy: bool,
        weak: &gpui_kit::WeakEntity<Self>,
        muted: Hsla,
    ) -> impl IntoElement {
        // The **whole row** is the click target — the `Checkbox` is visual only
        // (its own handler would fire *in addition* to the row's, cancelling out).
        let row_weak = weak.clone();
        div()
            .id(id)
            .flex()
            .items_center()
            .gap(px(8.))
            .when(!busy, |el| {
                el.cursor_pointer()
                    .on_click(move |_, _window, cx| flip(&row_weak, which, cx))
            })
            .child(Checkbox::new((id, 1u32), checked).disabled(busy))
            .child(div().text_size(px(11.)).text_color(muted).child(label))
    }
}

#[derive(Clone, Copy)]
enum Attach {
    Dump,
    Log,
}

/// Toggle one of the attach checkboxes (unless a send is in flight).
fn flip(weak: &gpui_kit::WeakEntity<ReportWindow>, which: Attach, cx: &mut App) {
    weak.update(cx, |this, cx| {
        if this.state.busy() {
            return;
        }
        match which {
            Attach::Dump => this.attach_dump = !this.attach_dump,
            Attach::Log => this.attach_log = !this.attach_log,
        }
        cx.notify();
    })
    .ok();
}

/// Clamp to `max` chars (on a char boundary), appending `…` if it was cut.
fn truncate(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((byte_idx, _)) => format!("{}…", &s[..byte_idx]),
        None => s.to_string(),
    }
}

fn detail_row(label: String, value: String, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .flex_none()
        // The scroll container is a flex_col, which stretches children to
        // its own (viewport) width by default — that clips a wide row
        // instead of letting it overflow, which is exactly what needs to
        // happen for the container's `overflow_x_scroll` to have anything
        // to scroll *to*. `self_start()` opts this row out of that stretch,
        // sizing it to its own (possibly wider) content instead.
        .self_start()
        .gap(px(8.))
        .text_size(px(12.))
        .child(
            div()
                .flex_none()
                .w(px(84.))
                .text_color(muted)
                .child(label),
        )
        // A location/address/thread value is one technical token (a path,
        // not prose) — keep it on one line rather than wrapping mid-path;
        // the detail card's own scroll (see `Render for ReportWindow`)
        // handles the overflow that leaves for a long one.
        .child(div().flex_none().whitespace_nowrap().child(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(panic_msg: &str) -> Digest {
        Digest {
            reason: String::new(),
            address: String::new(),
            crashing_tid: String::new(),
            panic_msg: panic_msg.to_string(),
            stack: String::new(),
            frames: Vec::new(),
            dump_path: String::new(),
            os_build: String::new(),
            minidump: Vec::new(),
        }
    }

    /// The reporter is its own process with no log, so a missing key would just show the raw key name to the user:
    /// every `crash-report.*` key this file uses must exist in every catalog — and the hints strings must keep the
    /// placeholders the code fills in.
    #[test]
    fn every_key_the_dialog_uses_exists_in_every_catalog() {
        let source = include_str!("crash_report.rs");
        let mut keys: Vec<String> = Vec::new();
        for (i, _) in source.match_indices("\"crash-report.") {
            let rest = &source[i + 1..];
            let end = rest.find('"').unwrap();
            // (The scan also sees this test's own `"crash-report."` literals — those have no key after the dot.)
            if rest[..end].len() > "crash-report.".len() {
                keys.push(rest[..end].to_owned());
            }
        }
        assert!(keys.len() > 20, "found only {} keys — the scan is broken", keys.len());

        let catalogs = [
            ("de-DE", include_str!("../locales/de-DE.toml")),
            ("en-US", include_str!("../locales/en-US.toml")),
        ];
        let placeholders = [
            ("hints-running", &["{done}", "{total}"][..]),
            ("hints-done", &["{resolved}", "{total}"][..]),
            ("hints-skipped", &["{resolved}", "{total}"][..]),
            ("hints-failed-error", &["{reason}"][..]),
        ];
        for (tag, src) in catalogs {
            let table: toml::Table = src.parse().unwrap();
            let section = table["crash-report"].as_table().unwrap_or_else(|| panic!("{tag}: no [crash-report]"));
            for key in &keys {
                let name = key.strip_prefix("crash-report.").unwrap();
                assert!(section.get(name).and_then(|v| v.as_str()).is_some(), "{tag}: missing {key}");
            }
            for (name, wanted) in placeholders {
                let text = section[name].as_str().unwrap();
                for p in wanted {
                    assert!(text.contains(p), "{tag}.{name} lacks {p}: {text}");
                }
            }
        }
    }

    #[test]
    fn panic_message_shows_the_payload_and_lifts_the_location() {
        let s = Summary::build(&digest(
            "panicked at crates/dtb-ke-ui/src/main.rs:129:73:\nDTB_KE_CRASH_TEST: deliberate panic",
        ));
        assert_eq!(
            s.message.as_deref(),
            Some("DTB_KE_CRASH_TEST: deliberate panic")
        );
        assert!(
            s.rows
                .iter()
                .any(|(k, v)| *k == Row::Location && v == "crates/dtb-ke-ui/src/main.rs:129:73")
        );
    }

    #[test]
    fn no_message_or_location_for_a_non_panic() {
        let s = Summary::build(&digest(""));
        assert_eq!(s.message, None);
        assert!(s.rows.iter().all(|(k, _)| *k != Row::Location));
    }

    #[test]
    fn a_long_multiline_payload_is_flattened_and_capped() {
        let s = Summary::build(&digest(&format!(
            "panicked at a.rs:1:1:\nassertion failed\n{}",
            "x".repeat(400)
        )));
        let m = s.message.unwrap();
        assert!(m.starts_with("assertion failed x"));
        assert!(m.ends_with('…'));
        assert!(m.chars().count() <= 241);
    }
}

// ── iOS: a crash from the previous launch, offered as a sheet ─────────────
//
// There is no helper process on iOS. The crashed run left a `.crash` snapshot (plus a `.session`
// sidecar) under `logs/crashes/`; the *next* launch converts each into a `.dtbkedmp` — the same artifact
// the desktop helper writes — and offers to send the newest one in the reporter view, hosted as a
// sheet.

/// A converted crash, ready to show.
struct Ready {
    digest: Digest,
    dmp_path: std::path::PathBuf,
    crashed_at: std::time::SystemTime,
}

fn crash_dir() -> std::path::PathBuf {
    crate::filesystem::FilesystemHelper::instance().get_log_dir().join("crashes")
}

/// Turn every pending snapshot into a `.dtbkedmp` (sources are removed only once the dump is on disk) and
/// drop the sidecars of runs that ended normally. Blocking file I/O — run it off the main thread.
fn convert_pending(dir: &std::path::Path, own_session: Option<std::path::PathBuf>) -> Vec<Ready> {
    use dtb_ke_crash::snapshot;
    let mut ready = Vec::new();
    for pending in snapshot::pending::find(dir) {
        let crash = match std::fs::read(&pending.crash) {
            Ok(bytes) => bytes,
            Err(err) => {
                log::warn!("cannot read crash snapshot {}: {err}", pending.crash.display());
                continue;
            }
        };
        let session = std::fs::read(&pending.session).ok();
        let snap = match snapshot::parse(&crash, session.as_deref()) {
            Ok(snap) => snap,
            Err(err) => {
                log::warn!("discarding unreadable crash snapshot {}: {err}", pending.crash.display());
                snapshot::pending::remove(&pending);
                continue;
            }
        };
        let dmp_path = pending.crash.with_extension(dtb_ke_crash::DUMP_EXTENSION);
        if let Err(err) = std::fs::write(&dmp_path, snapshot::to_minidump(&snap)) {
            log::error!("cannot write {}: {err}", dmp_path.display());
            continue;
        }
        log::info!(
            "converted crash snapshot {} → {} ({})",
            pending.crash.display(),
            dmp_path.display(),
            if snap.complete { "complete" } else { "truncated" }
        );
        snapshot::pending::remove(&pending);
        ready.push(Ready {
            digest: digest_of(&snap),
            dmp_path,
            crashed_at: std::time::UNIX_EPOCH + std::time::Duration::from_secs(snap.crash_time_unix),
        });
    }
    snapshot::pending::remove_stale_sessions(dir, own_session.as_deref());
    ready
}

/// The reporter's [`Digest`] for a snapshot — the same fields the desktop helper pipes in, with the
/// stack as `module +offset` frames plus the app's own images (UUIDs, for offline symbolication).
fn digest_of(snap: &dtb_ke_crash::snapshot::CrashSnapshotDTO) -> Digest {
    let d = dtb_ke_crash::snapshot::summary::digest(snap);
    let thread_name = snap
        .threads
        .iter()
        .find(|t| t.id == d.crashing_thread)
        .map(|t| t.name.as_str())
        .filter(|n| !n.is_empty());
    let mut stack = d.stack;
    stack.push_str("\nModules:\n");
    for m in snap.session.modules.iter().filter(|m| m.path.contains(".app/")) {
        let uuid: String = m.uuid.iter().map(|b| format!("{b:02X}")).collect();
        stack.push_str(&format!("{:#018x}-{:#018x} {uuid} {}\n", m.base, m.base + m.size, m.path));
    }
    Digest {
        reason: d.reason,
        address: if d.address == 0 { String::new() } else { format!("{:#018x}", d.address) },
        crashing_tid: match thread_name {
            Some(name) => format!("{name} ({:#x})", d.crashing_thread),
            None => format!("{:#x}", d.crashing_thread),
        },
        panic_msg: d.panic_message,
        stack,
        frames: snapshot_frames(snap),
        dump_path: String::new(),
        os_build: snap.session.os_build.clone(),
        minidump: Vec::new(),
    }
}

/// Every thread's frames of a snapshot as (image UUID, offset) — the input of the system-symbol hints. A caller
/// frame's address is a return address, so its *call instruction* (one byte earlier) is what names the function.
fn snapshot_frames(snap: &dtb_ke_crash::snapshot::CrashSnapshotDTO) -> Vec<dtb_ke_crash::syshints::FrameRef> {
    use dtb_ke_crash::snapshot::summary::{module_for, walk};
    let mut out = Vec::new();
    for thread in &snap.threads {
        for (i, pc) in walk(thread).into_iter().enumerate() {
            let at = if i == 0 { pc } else { pc.saturating_sub(1) };
            if let Some(m) = module_for(&snap.session, at) {
                out.push(dtb_ke_crash::syshints::FrameRef {
                    thread: thread.id,
                    uuid: m.uuid,
                    offset: at - m.base,
                    path: m.path.clone(),
                });
            }
        }
    }
    out
}

/// Convert any crash left by a previous launch and offer to send the newest. Call once, a moment after
/// startup, when the main window (which hosts the sheet) exists.
pub fn offer_pending(cx: &mut App) {
    let dir = crash_dir();
    #[cfg(target_os = "ios")]
    let own = dtb_ke_crash::ios::own_session_path(&dir, "DTB-KE");
    #[cfg(not(target_os = "ios"))]
    let own = None;

    cx.spawn(async move |cx| {
        let ready = cx
            .background_executor()
            .spawn(async move { convert_pending(&dir, own) })
            .await;
        let Some(newest) = ready.into_iter().last() else { return };
        cx.update(|cx| present_ready(newest, cx));
    })
    .detach();
}

fn present_ready(ready: Ready, cx: &mut App) {
    let Ready { mut digest, dmp_path, crashed_at } = ready;
    digest.minidump = std::fs::read(&dmp_path).unwrap_or_default();
    digest.dump_path = dmp_path.to_string_lossy().into_owned();
    // The next launch's own log is newer than the crash; take the newest one written up to then.
    let log = newest_log_before(Some(crashed_at + std::time::Duration::from_secs(2)));
    // Delivered → the `.dtbkedmp` has served its purpose. Any other answer keeps it, as on the desktop.
    open_reporter(
        digest,
        log,
        false,
        Box::new(move |code| {
            if code == report::SENT {
                if let Err(err) = std::fs::remove_file(&dmp_path) {
                    log::warn!("cannot remove {}: {err}", dmp_path.display());
                }
            }
        }),
        cx,
    );
}

/// Show the reporter for `digest` in the running app — a sheet where secondary windows are sheets, a
/// window elsewhere. `on_close` sees the verdict (`report::*`) before the view goes away.
fn open_reporter(
    digest: Digest,
    log: Vec<u8>,
    simulate_send: bool,
    on_close: Box<dyn Fn(i32)>,
    cx: &mut App,
) {
    let hints = start_hints(&digest);
    // Whatever closes the report (any verdict) first stops the run and saves what it found into the kept dump.
    let on_close: Box<dyn Fn(i32)> = {
        let hints = hints.clone();
        Box::new(move |code| {
            finish_hints(&hints, code);
            on_close(code);
        })
    };
    let summary = Summary::build(&digest);
    let (subject, body) = compose(&digest);
    let subject = if simulate_send {
        format!("[Test] {subject}")
    } else {
        crate::debug::crash::mark_subject(subject)
    };
    let dmp = digest.minidump;
    let mut options = window_options(cx);

    if crate::sheet::enabled() {
        // The desktop window title is too long for a sheet header.
        if let Some(titlebar) = options.titlebar.as_mut() {
            titlebar.title = Some(cx.t("crash-report.sheet-title"));
        }
        let verdict: Verdict = std::rc::Rc::new(move |code, cx| {
            on_close(code);
            crate::sheet::dismiss(cx);
        });
        crate::sheet::present(cx, "crash-report", &options, move |window, cx| {
            cx.new(|cx| {
                ReportWindow::new(summary, subject, body, dmp, log, false, verdict, hints.clone(), simulate_send, window, cx)
            })
        });
        return;
    }

    let handle_cell: std::rc::Rc<std::cell::Cell<Option<gpui_kit::AnyWindowHandle>>> = Default::default();
    let verdict: Verdict = {
        let handle_cell = handle_cell.clone();
        std::rc::Rc::new(move |code, cx| {
            on_close(code);
            if let Some(handle) = handle_cell.get() {
                handle.update(cx, |_, window, _| window.remove_window()).ok();
            }
        })
    };
    match cx.open_window(options, move |window, cx| {
        cx.new(|cx| ReportWindow::new(summary, subject, body, dmp, log, false, verdict, hints.clone(), simulate_send, window, cx))
    }) {
        Ok(handle) => handle_cell.set(Some(handle.into())),
        Err(err) => log::error!("cannot open the crash report window: {err}"),
    }
}

/// Developer option: show the reporter with sample data. "Sending" is simulated — nothing is mailed.
pub fn show_sample(cx: &mut App) {
    let digest = Digest {
        reason: "EXC_BAD_ACCESS / KERN_INVALID_ADDRESS (Beispiel)".into(),
        address: "0x0000000000000010".into(),
        crashing_tid: "main (0x103)".into(),
        panic_msg: "panicked at crates/dtb-ke-ui/src/beispiel.rs:42:9:\nBeispielhafter Fehler für die Vorschau".into(),
        stack: "#0 DTB-Kampfrichtereinsatzplaene +0x1234\n#1 DTB-Kampfrichtereinsatzplaene +0x5678\n\nModules:\n(Beispiel)\n".into(),
        frames: Vec::new(),
        dump_path: String::new(),
        os_build: String::new(),
        minidump: vec![0; 2048],
    };
    open_reporter(digest, b"Beispiel-Protokoll\n".to_vec(), true, Box::new(|_| {}), cx);
}
