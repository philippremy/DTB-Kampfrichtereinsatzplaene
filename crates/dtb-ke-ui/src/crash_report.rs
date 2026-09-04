//! The crash reporter — a second, minimal mode of this same binary.
//!
//! When the app faults, the out-of-process crash helper (`dtb-ke-crash`) writes
//! a minidump, then **relaunches this executable** and streams us, on stdin, a
//! small digest followed by the raw dump —
//! `reason \0 address \0 crashing_tid \0 panic_msg \0 stack \0 <minidump bytes>`.
//! We recognise that we are the reporter by our *parent process*: the helper
//! (`getppid()` → a binary named [`dtb_ke_crash::HELPER_FILE_NAME`]). No CLI
//! flag, no environment variable. "Bericht senden" mails the digest + stack (and
//! optionally the `.dmp`) via [`crate::mail`]; the `.dmp` is attached only if the
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

use gpui::Subscription;
use gpui::{
    App, AppContext, Bounds, Context, FontWeight, Hsla, InteractiveElement, IntoElement,
    ParentElement, Render, Size, StatefulInteractiveElement, Styled, TitlebarOptions, Window,
    WindowBounds, WindowKind, WindowOptions, div, prelude::FluentBuilder, px,
};

use dtb_ke_crash::report;

use crate::build_info;
use crate::components::checkbox::Checkbox;
use crate::components::{Button, ButtonTone, Icon, Spinner};
use crate::mail;
use crate::theme::{ActiveTheme, Appearance, Theme, ThemeMode};

const INTRO: &str = "DTB Kampfrichtereinsatzpläne ist abgestürzt. Es kann ein Fehlerbericht zur Problembehebung gesendet werden. \
Der Bericht enthält ausschließlich technische Angaben zum Absturz (Speicheradressen, geladene Programmbibliotheken, Funktionsstack), \
keine personenbezogenen oder wettkampfbezogene Daten.";

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
            "--- REPORT ---\nSubject: {subject}\n\n{body}\n--- dmp: {} B · log: {} B · mail::available = {} ---",
            digest.minidump.len(),
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

    let summary = Summary::build(&digest);
    let (subject, body) = compose(&digest);
    let dmp = digest.minidump;
    let log = newest_log();

    // The verdict is delivered by `std::process::exit(report::*)` straight from
    // the button / window-close handlers — **not** by returning it here.
    // `cx.quit()` on macOS is `[NSApp terminate:]`, which calls `exit(0)`
    // internally, so anything after `application().run()` (and any exit code we'd
    // return) is unreachable — and `exit(0)` would read as `SENT`, silently
    // discarding every dump.
    gpui_platform::application().run(move |cx: &mut App| {
        // macOS: don't show a second dock icon for the reporter.
        #[cfg(target_os = "macos")]
        {
            use objc2::MainThreadMarker;
            use objc2_app_kit::{NSApp, NSApplicationActivationPolicy};
            let mtm = MainThreadMarker::new()
                .expect("on_finish_launching always runs on the Main Thread");
            NSApp(mtm).setActivationPolicy(NSApplicationActivationPolicy::Accessory);
        }
        gpui_base::init(cx);
        Theme::install(
            ThemeMode::System,
            Appearance::from(cx.window_appearance()),
            cx,
        );
        // Closing the window == declining.
        cx.on_window_closed(|_, _| std::process::exit(report::DECLINED))
            .detach();

        let opts = window_options(cx);
        let opened = cx.open_window(opts, move |window, cx| {
            cx.new(|cx| ReportWindow::new(summary, subject, body, dmp, log, autosend, window, cx))
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
    const CAP: usize = 1 << 22;
    let Ok(dir) = std::fs::read_dir(crate::filesystem::FilesystemHelper::instance().get_log_dir())
    else {
        return Vec::new();
    };
    let newest = dir
        .flatten()
        .filter(|e| e.path().extension().is_some_and(|x| x == "log"))
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
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
/// `panic_msg` `\0` `stack` `\0` `<minidump bytes>`.
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
    /// The raw minidump — attached to the report only if the user opts in.
    minidump: Vec<u8>,
}

fn read_digest_from_stdin() -> Option<Digest> {
    use std::io::Read;
    let mut buf = Vec::new();
    std::io::stdin().read_to_end(&mut buf).ok()?;

    let mut fields = buf.splitn(6, |&b| b == 0);
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
    let minidump = fields.next().map(<[u8]>::to_vec).unwrap_or_default();
    Some(Digest {
        reason,
        address,
        crashing_tid,
        panic_msg,
        stack,
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
            title: Some("DTB-Kampfrichtereinsatzpläne ist abgestürzt".into()),
            appears_transparent: true,
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_resizable: false,
        is_minimizable: false,
        ..Default::default()
    }
}

// ── the pre-digested view of a dump ──────────────────────────────────────

#[derive(Clone)]
struct Summary {
    kind: String,
    message: Option<String>,
    rows: Vec<(&'static str, String)>,
}

impl Summary {
    fn build(d: &Digest) -> Self {
        let is_panic = !d.panic_msg.trim().is_empty();
        let kind = if is_panic {
            "Programmfehler (Panic)".to_string()
        } else if d.reason.is_empty() {
            "Absturz".to_string()
        } else {
            d.reason.clone()
        };

        let mut rows: Vec<(&'static str, String)> = Vec::new();

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
                    rows.push(("Ort", loc.to_string()));
                }
                let payload = payload.trim().replace('\n', " ");
                Some(truncate(&payload, 240))
            }
        };

        // Show the technical reason for a panic too (as a row, since `kind` is
        // the friendlier label there).
        if is_panic && !d.reason.is_empty() {
            rows.push(("Signal", d.reason.clone()));
        }
        if !d.address.is_empty() && d.address != "0x0000000000000000" {
            rows.push(("Adresse", d.address.clone()));
        }
        if !d.crashing_tid.is_empty() {
            rows.push(("Thread", d.crashing_tid.clone()));
        }
        rows.push((
            "Zeitpunkt",
            chrono::Local::now().format("%d.%m.%Y %H:%M:%S").to_string(),
        ));

        Self {
            kind,
            message,
            rows,
        }
    }
}

/// The e-mail subject + plain-text body for a report. The body carries only the
/// digest + the symbol-free stack (no process memory) — the raw `.dmp` is a
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

struct ReportWindow {
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

        Self {
            summary,
            subject,
            body,
            dmp,
            log,
            attach_dump: false,
            attach_log: false,
            state: SendState::Idle,
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

        let mut attachments = Vec::new();
        if self.attach_dump {
            attachments.push((
                "crash.dmp".to_owned(),
                "application/x-dmp",
                self.dmp.clone(),
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
            body: self.body.clone(),
            attachments,
        };
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { mail::send(report) })
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
                    std::process::exit(report::SENT);
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
                    .child("DTB Kampfrichtereinsatzpläne wurde unerwartet beendet :("),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(INTRO),
            )
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .gap(px(6.))
                    .p(px(12.))
                    .bg(c.surface)
                    .border_1()
                    .border_color(c.border)
                    .rounded(radius)
                    .child(
                        div()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(13.))
                            .child(s.kind.clone()),
                    )
                    .when_some(s.message.clone(), |el, m| {
                        el.child(div().text_size(px(12.)).text_color(c.foreground).child(m))
                    })
                    .children(
                        s.rows
                            .iter()
                            .map(|(k, v)| detail_row(k, v, c.muted_foreground)),
                    ),
            )
            .child(self.footer(cx))
    }
}

impl ReportWindow {
    fn footer(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let weak = cx.entity().downgrade();

        // Transport not compiled in → nothing to do but close.
        if !mail::available() {
            return div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(
                    div()
                        .text_size(px(11.))
                        .text_color(c.muted_foreground)
                        .child(
                            "Der Versand von Fehlerberichten ist in dieser Programmversion nicht möglich. \
                             Das Speicherabbild liegt unter „Protokolle“.",
                        ),
                )
                .child(
                    div().flex().justify_end().child(
                        Button::new("cr-close", "Schließen")
                            .on_click(|_, _, _| std::process::exit(report::DECLINED)),
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
                    .child("Gesendet! Vielen Dank."),
            );
        }

        let busy = self.state.busy();
        let failed = matches!(self.state, SendState::Failed(_));

        let dump_row = self.attach_row(
            "cr-attach-dump",
            "Speicherabbild anhängen (Kann Teilwettkampfdaten enthalten)",
            self.attach_dump,
            Attach::Dump,
            busy,
            &weak,
            c.muted_foreground,
        );
        let log_row = (!self.log.is_empty()).then(|| {
            self.attach_row(
                "cr-attach-log",
                "Protokoll anhängen (Kann Wettkampfnamen enthalten)",
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
            if failed {
                "Erneut senden"
            } else {
                "Bericht senden"
            },
        )
        .tone(ButtonTone::Primary)
        .disabled(busy)
        .on_click(move |_, _, cx| {
            send_weak.update(cx, |this, cx| this.send(cx)).ok();
        });

        let left_btn = if failed {
            Button::new("cr-keep", "Nur lokal behalten")
                .on_click(|_, _, _| std::process::exit(report::QUEUED))
        } else {
            Button::new("cr-decline", "Nicht senden")
                .disabled(busy)
                .on_click(|_, _, _| std::process::exit(report::DECLINED))
        };

        div()
            .flex()
            .flex_col()
            .gap(px(6.))
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
                                .child("Wird gesendet …"),
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
        label: &'static str,
        checked: bool,
        which: Attach,
        busy: bool,
        weak: &gpui::WeakEntity<Self>,
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
fn flip(weak: &gpui::WeakEntity<ReportWindow>, which: Attach, cx: &mut App) {
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

fn detail_row(label: &str, value: &str, muted: Hsla) -> impl IntoElement {
    div()
        .flex()
        .gap(px(8.))
        .text_size(px(12.))
        .child(
            div()
                .flex_none()
                .w(px(84.))
                .text_color(muted)
                .child(label.to_string()),
        )
        .child(div().child(value.to_string()))
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
            minidump: Vec::new(),
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
                .any(|(k, v)| *k == "Ort" && v == "crates/dtb-ke-ui/src/main.rs:129:73")
        );
    }

    #[test]
    fn no_message_or_location_for_a_non_panic() {
        let s = Summary::build(&digest(""));
        assert_eq!(s.message, None);
        assert!(s.rows.iter().all(|(k, _)| *k != "Ort"));
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
