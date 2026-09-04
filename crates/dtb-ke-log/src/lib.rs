//! The application logger.
//!
//! One global [`DTBKELogger`] writes every record straight to the session log
//! file (unbuffered — no background flush thread) and, in debug builds, mirrors
//! it to `stderr` (optionally coloured).
//!
//! The **level filter is tunable at runtime** via [`set_level`] — the settings
//! window uses this so a change takes effect immediately, even long after
//! [`DTBKELogger::register`]. [`set_level(None)`](set_level) reverts to the
//! automatic choice (debug/release default, overridable once at startup with the
//! `DTB_KE_LOG_LEVEL` env var).
//!
//! [`parse_line`] recovers the fields of a written line — the log-viewer window
//! uses it to colourise.
//!
//! [`fault!`] is a signal-safe variant for the crash handlers: it allocates
//! nothing and takes no lock, writing one `[F]` line straight to the log file's
//! raw descriptor (and, in debug builds, the raw stderr descriptor, with the
//! `F` and message in red).

use std::{
    cell::Cell,
    fmt::{Display, Write as _},
    fs::File,
    io::{IsTerminal, Write},
    path::Path,
    str::FromStr,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU8, AtomicU16, Ordering},
    },
    time::Instant,
};

use colored::Colorize;
use log::{Level, LevelFilter, Log};

static DTB_KE_GLOBAL_LOGGER: OnceLock<DTBKELogger> = OnceLock::new();
static DTB_KE_GLOBAL_THREAD_COUNT: AtomicU16 = AtomicU16::new(0);

thread_local! {
    /// This thread's number in the `Main Thread` / `DTB-KE-T NN` scheme.
    /// `u16::MAX` until the thread first logs. `const`-initialised so a read is a
    /// plain static-TLS load — safe from a signal handler ([`fault!`]).
    static THREAD_NO: Cell<u16> = const { Cell::new(u16::MAX) };
}

/// Runtime level override. `0` = follow [`auto_level`]; `1..=5` = a
/// [`LevelFilter`] discriminant (`Error`..=`Trace`).
static LEVEL_OVERRIDE: AtomicU8 = AtomicU8::new(0);
/// The automatic level, resolved once from the build profile + `DTB_KE_LOG_LEVEL`.
static AUTO_LEVEL: OnceLock<LevelFilter> = OnceLock::new();

/// The build-time default level (debug → `Debug`, release → `Info`), with a
/// one-shot `DTB_KE_LOG_LEVEL` env override.
fn auto_level() -> LevelFilter {
    *AUTO_LEVEL.get_or_init(|| {
        let mut level = if cfg!(debug_assertions) {
            LevelFilter::Debug
        } else {
            LevelFilter::Info
        };
        if let Ok(raw) = std::env::var("DTB_KE_LOG_LEVEL") {
            match LevelFilter::from_str(raw.trim()) {
                Ok(parsed) => level = parsed,
                Err(_) => eprintln!("dtb-ke-log: ignoring invalid DTB_KE_LOG_LEVEL={raw:?}"),
            }
        }
        level
    })
}

/// The level filter currently in effect (override, else [`auto_level`]).
pub fn effective_level() -> LevelFilter {
    match LEVEL_OVERRIDE.load(Ordering::Relaxed) {
        1 => LevelFilter::Error,
        2 => LevelFilter::Warn,
        3 => LevelFilter::Info,
        4 => LevelFilter::Debug,
        5 => LevelFilter::Trace,
        _ => auto_level(),
    }
}

/// Whether a level override is currently active (vs. following auto-detection).
pub fn level_is_overridden() -> bool {
    LEVEL_OVERRIDE.load(Ordering::Relaxed) != 0
}

/// Set (or, with `None`, clear) the runtime level override. Takes effect
/// immediately, including while the logger is already running.
pub fn set_level(level: Option<LevelFilter>) {
    let code = match level {
        Some(LevelFilter::Error) => 1,
        Some(LevelFilter::Warn) => 2,
        Some(LevelFilter::Info) => 3,
        Some(LevelFilter::Debug) => 4,
        Some(LevelFilter::Trace) => 5,
        Some(LevelFilter::Off) | None => 0,
    };
    LEVEL_OVERRIDE.store(code, Ordering::Relaxed);
    log::set_max_level(effective_level());
}

/// A logger that writes to a file and, in debug builds, mirrors to `stderr`.
pub struct DTBKELogger {
    is_debug: bool,
    stderr_color: bool,
    restrict_targets: bool,
    file: Mutex<File>,
    /// The session file's raw descriptor / handle (as `i64`), captured before
    /// `file` went behind the `Mutex`. [`fault!`] writes through this directly so
    /// it never has to `lock()` — `-1` if unknown. The `File` behind the `Mutex`
    /// keeps it open; we only ever borrow it here.
    fault_fd: i64,
    startup_time: Instant,
}

impl DTBKELogger {
    /// Create a logger writing to the file at `with_file` (created / appended).
    pub fn new(with_file: impl AsRef<Path>) -> Result<Self, String> {
        let is_debug = cfg!(debug_assertions);

        let stderr_color = std::env::var_os("NO_COLOR").is_none()
            && (std::env::var_os("FORCE_COLOR").is_some()
                || std::env::var_os("CLICOLOR_FORCE").is_some()
                || (std::io::stderr().is_terminal()
                    && std::env::var("TERM").is_ok_and(|term| term != "dumb")));

        let mut restrict_targets = !is_debug;
        if let Ok(raw) = std::env::var("DTB_KE_RESTRICT_TARGETS") {
            match raw.parse::<bool>() {
                Ok(parsed) => restrict_targets = parsed,
                Err(_) => {
                    eprintln!("dtb-ke-log: ignoring invalid DTB_KE_RESTRICT_TARGETS={raw:?}")
                }
            }
        }

        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(with_file.as_ref())
            .map_err(|io_err| {
                format!(
                    "Cannot open file {} for logging, I/O error: {io_err}!",
                    with_file.as_ref().display(),
                )
            })?;

        let fault_fd = raw::of(&file);

        Ok(Self {
            is_debug,
            stderr_color,
            restrict_targets,
            file: Mutex::new(file),
            fault_fd,
            startup_time: Instant::now(),
        })
    }

    /// Register as the global `log` logger. Idempotent-safe only once per
    /// process (panics if another logger is already set).
    pub fn register(self) {
        let static_self = DTB_KE_GLOBAL_LOGGER.get_or_init(|| self);
        log::set_logger(static_self).expect("Another logger has already been set!");
        log::set_max_level(effective_level());
    }
}

impl Log for DTBKELogger {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        if metadata.level() > effective_level() {
            return false;
        }
        if !self.restrict_targets {
            return true;
        }
        is_own_target(metadata.target())
    }

    fn log(&self, record: &log::Record) {
        if !self.enabled(record.metadata()) {
            return;
        }

        let timecode_fmt = format!(
            "{:>14.6}",
            Instant::now()
                .duration_since(self.startup_time)
                .as_secs_f32()
        );

        let log_level_fmt = record
            .level()
            .to_string()
            .chars()
            .next()
            .expect("Level always has a first char")
            .to_ascii_uppercase()
            .to_string();

        let thread_num = match THREAD_NO.get() {
            u16::MAX => {
                THREAD_NO.set(DTB_KE_GLOBAL_THREAD_COUNT.fetch_add(1, Ordering::SeqCst));
                THREAD_NO.get()
            }
            initialized => initialized,
        };
        let thread_fmt = match thread_num {
            0 => "Main Thread".to_owned(),
            other => format!("DTB-KE-T {other:0>2}"),
        };

        let subsystem_fmt = record
            .metadata()
            .target()
            .replace("DTB_Kampfrichtereinsatzpläne", "dtb_ke_ui");

        let log_call = |sink: &mut dyn Write,
                        timecode: &dyn Display,
                        level: &dyn Display,
                        thread: &dyn Display,
                        subsystem: &dyn Display| {
            let fmt_string = format!(
                "[{timecode}] [{level}] [{thread}] [{subsystem}] {}\n",
                record.args()
            );
            sink.write_all(fmt_string.as_bytes()).ok()
        };

        if self.is_debug {
            let mut std_err = std::io::stderr().lock();
            if self.stderr_color {
                let timecode_fmt = timecode_fmt.bright_black();
                let log_level_fmt = match record.level() {
                    Level::Error => log_level_fmt.red(),
                    Level::Warn => log_level_fmt.yellow(),
                    Level::Info => log_level_fmt.green(),
                    Level::Debug => log_level_fmt.blue(),
                    Level::Trace => log_level_fmt.white(),
                };
                let thread_fmt = thread_fmt.bright_black();
                let subsystem_fmt = subsystem_fmt.bright_black();
                log_call(
                    &mut std_err,
                    &timecode_fmt,
                    &log_level_fmt,
                    &thread_fmt,
                    &subsystem_fmt,
                );
            } else {
                log_call(
                    &mut std_err,
                    &timecode_fmt,
                    &log_level_fmt,
                    &thread_fmt,
                    &subsystem_fmt,
                );
            }
        }

        // Straight to the file — no BufWriter, so a crash never loses the tail
        // and no flush thread is needed.
        if let Ok(mut file) = self.file.lock() {
            log_call(
                &mut *file,
                &timecode_fmt,
                &log_level_fmt,
                &thread_fmt,
                &subsystem_fmt,
            );
        }
    }

    fn flush(&self) {
        if self.is_debug {
            std::io::stderr().flush().ok();
        }
        if let Ok(mut file) = self.file.lock() {
            file.flush().ok();
        }
    }
}

impl Drop for DTBKELogger {
    fn drop(&mut self) {
        self.flush();
    }
}

// ── fault!  — the signal-safe log path ─────────────────────────────────────

/// Log one `[F]` (fault) line — **safe to call from a signal / exception
/// handler**.
///
/// Unlike the `log::*` macros this allocates nothing and takes no lock: it
/// renders into a fixed stack buffer and writes it with a single raw `write`
/// (append-mode, so it interleaves atomically with the normal log path) to the
/// session file's descriptor, and — in debug builds — to the raw stderr
/// descriptor with the `F` and the message in red.
///
/// The line keeps the house format
/// `[<time>] [F] [<thread>] [<module>] <message>`; the thread is the same
/// `Main Thread` / `DTB-KE-T NN` scheme as the rest of the log (`?` for a thread
/// that has never logged), the module is `module_path!()` at the call site.
///
/// **Keep the arguments trivial** — `&str`, integers, `{:#x}` — so formatting
/// them can't allocate or re-enter a lock.
#[macro_export]
macro_rules! fault {
    ($($arg:tt)*) => {
        $crate::__fault(::core::module_path!(), ::core::format_args!($($arg)*))
    };
}

/// Implementation detail of [`fault!`] — not a stable API.
#[doc(hidden)]
pub fn __fault(module: &str, args: std::fmt::Arguments<'_>) {
    let logger = DTB_KE_GLOBAL_LOGGER.get();
    let secs = logger
        .map(|l| Instant::now().duration_since(l.startup_time).as_secs_f32())
        .unwrap_or(0.0);
    let thread_no = THREAD_NO.get();

    // One reusable stack buffer — a stack-overflow crash may reach here with
    // little room to spare.
    let mut line = FaultLine::new();

    // 1. the session file, plain.
    render_fault(&mut line, false, secs, thread_no, module, args);
    let file_fd = logger.map(|l| l.fault_fd).unwrap_or(raw::NONE);
    raw::write(file_fd, line.finish());

    // 2. the debug stderr mirror, `F` + message in red when colour is on.
    let debug = logger.map(|l| l.is_debug).unwrap_or(cfg!(debug_assertions));
    if debug {
        line.clear();
        let colour = logger.is_some_and(|l| l.stderr_color);
        render_fault(&mut line, colour, secs, thread_no, module, args);
        raw::write(raw::current_stderr(), line.finish());
    }
}

/// Render one fault line into `out`. `colour` wraps the timecode / thread /
/// module in dim and the `F` + message in red (ANSI).
fn render_fault(
    out: &mut FaultLine,
    colour: bool,
    secs: f32,
    thread_no: u16,
    module: &str,
    args: std::fmt::Arguments<'_>,
) {
    let (dim, red, rst) = if colour {
        ("\x1b[90m", "\x1b[31m", "\x1b[0m")
    } else {
        ("", "", "")
    };
    let _ = write!(out, "[{dim}{secs:>14.6}{rst}] [{red}F{rst}] [{dim}");
    match thread_no {
        0 => {
            let _ = out.write_str("Main Thread");
        }
        u16::MAX => {
            let _ = out.write_str("?");
        }
        n => {
            let _ = write!(out, "DTB-KE-T {n:0>2}");
        }
    }
    let _ = write!(out, "{rst}] [{dim}{module}{rst}] {red}");
    let _ = out.write_fmt(args);
    let _ = write!(out, "{rst}");
}

/// A fixed-capacity line buffer that silently truncates — a fault line is
/// best-effort, and this never allocates.
struct FaultLine {
    buf: [u8; Self::CAP],
    len: usize,
}

impl FaultLine {
    const CAP: usize = 1024;

    fn new() -> Self {
        Self {
            buf: [0; Self::CAP],
            len: 0,
        }
    }

    fn clear(&mut self) {
        self.len = 0;
    }

    /// The rendered bytes, guaranteed to end in exactly one `\n`.
    fn finish(&mut self) -> &[u8] {
        if self.len == Self::CAP {
            self.buf[Self::CAP - 1] = b'\n';
        } else {
            self.buf[self.len] = b'\n';
            self.len += 1;
        }
        &self.buf[..self.len]
    }
}

impl std::fmt::Write for FaultLine {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        let room = Self::CAP.saturating_sub(self.len);
        let n = s.len().min(room);
        self.buf[self.len..self.len + n].copy_from_slice(&s.as_bytes()[..n]);
        self.len += n;
        Ok(())
    }
}

/// Raw, lock-free, alloc-free writes to a descriptor — via a non-owning
/// [`std::mem::ManuallyDrop<File>`] so no `close` ever happens.
#[cfg(unix)]
mod raw {
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::os::fd::{AsRawFd, FromRawFd, RawFd};

    pub const NONE: i64 = -1;

    pub fn of(f: &impl AsRawFd) -> i64 {
        f.as_raw_fd() as i64
    }

    pub fn current_stderr() -> i64 {
        std::io::stderr().as_raw_fd() as i64
    }

    pub fn write(fd: i64, bytes: &[u8]) {
        if fd < 0 {
            return;
        }
        let file = ManuallyDrop::new(unsafe { std::fs::File::from_raw_fd(fd as RawFd) });
        let mut sink: &std::fs::File = &file;
        let _ = sink.write_all(bytes);
    }
}

#[cfg(windows)]
mod raw {
    use std::io::Write;
    use std::mem::ManuallyDrop;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};

    pub const NONE: i64 = 0;

    pub fn of(f: &impl AsRawHandle) -> i64 {
        f.as_raw_handle() as isize as i64
    }

    pub fn current_stderr() -> i64 {
        std::io::stderr().as_raw_handle() as isize as i64
    }

    pub fn write(fd: i64, bytes: &[u8]) {
        if fd == 0 {
            return;
        }
        let file =
            ManuallyDrop::new(unsafe { std::fs::File::from_raw_handle(fd as isize as RawHandle) });
        let mut sink: &std::fs::File = &file;
        let _ = sink.write_all(bytes);
    }
}

#[cfg(not(any(unix, windows)))]
mod raw {
    pub const NONE: i64 = -1;
    pub fn of(_f: &std::fs::File) -> i64 {
        -1
    }
    pub fn current_stderr() -> i64 {
        -1
    }
    pub fn write(_fd: i64, _bytes: &[u8]) {}
}

/// The parsed fields of one written log line. Every `&str` is a sub-slice of the
/// line passed to [`parse_line`], so a caller can recover byte offsets by
/// pointer arithmetic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LogLine<'a> {
    pub timecode: &'a str,
    /// The raw level bracket contents (`"I"`, `"W"`, …).
    pub level_text: &'a str,
    /// `None` if the level character was not recognised.
    pub level: Option<Level>,
    pub thread: &'a str,
    pub subsystem: &'a str,
    pub message: &'a str,
}

/// Parse a line written by [`DTBKELogger::log`] or [`fault!`]:
/// `[{timecode}] [{L}] [{thread}] [{subsystem}] {message}`.
///
/// A `[F]` (fault) line parses with `level_text == "F"` and `level == None`
/// (there is no `log::Level` for it).
///
/// Returns `None` for a line that does not match the shape (a bare panic
/// message, a continuation line, …).
pub fn parse_line(line: &str) -> Option<LogLine<'_>> {
    let (timecode, rest) = bracket(line)?;
    let (level_text, rest) = bracket(rest)?;
    let (thread, rest) = bracket(rest)?;
    let (subsystem, rest) = bracket(rest)?;
    let message = rest.strip_prefix(' ').unwrap_or(rest);

    let level = match level_text.trim() {
        "E" => Some(Level::Error),
        "W" => Some(Level::Warn),
        "I" => Some(Level::Info),
        "D" => Some(Level::Debug),
        "T" => Some(Level::Trace),
        _ => None,
    };

    Some(LogLine {
        timecode,
        level_text,
        level,
        thread,
        subsystem,
        message,
    })
}

/// Whether a `log` target belongs to this application (used when
/// `restrict_targets` is on — release builds). Matches the workspace crates
/// (`dtb_ke_*` / `dtb-ke-*`) **and the GUI binary crate**, whose module path is
/// `DTB_Kampfrichtereinsatzpläne`, not `dtb_ke_ui`.
fn is_own_target(target: &str) -> bool {
    target.contains("dtb_ke") || target.contains("dtb-ke") || target.contains("Kampfrichtereinsatz")
}

/// Split a leading `[inner]` (tolerating one leading space) off `s`, returning
/// `(inner, remainder-after-']')`.
fn bracket(s: &str) -> Option<(&str, &str)> {
    let s = s.strip_prefix(' ').unwrap_or(s);
    let s = s.strip_prefix('[')?;
    let end = s.find(']')?;
    Some((&s[..end], &s[end + 1..]))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_written_line() {
        let line = "[      0.001389] [I] [Main Thread] [dtb_ke_ui] Logger initialized, logging to /x/y.log";
        let parsed = parse_line(line).expect("should parse");
        assert_eq!(parsed.timecode.trim(), "0.001389");
        assert_eq!(parsed.level, Some(Level::Info));
        assert_eq!(parsed.thread, "Main Thread");
        assert_eq!(parsed.subsystem, "dtb_ke_ui");
        assert_eq!(parsed.message, "Logger initialized, logging to /x/y.log");
        // Fields are sub-slices of the input.
        let base = line.as_ptr() as usize;
        let msg = parsed.message.as_ptr() as usize;
        assert!(msg > base && msg < base + line.len());
    }

    #[test]
    fn rejects_a_non_matching_line() {
        assert!(parse_line("thread 'main' panicked at src/foo.rs:1:1").is_none());
        assert!(parse_line("").is_none());
        assert!(parse_line("   continuation text").is_none());
    }

    #[test]
    fn own_target_matches_workspace_and_the_gui_binary() {
        assert!(is_own_target("dtb_ke_persist::db"));
        assert!(is_own_target("dtb-ke-export"));
        // The GUI binary crate — module path from its `[[bin]]` name.
        assert!(is_own_target("DTB_Kampfrichtereinsatzpläne::about"));
        assert!(!is_own_target("gpui_macos::text_system"));
        assert!(!is_own_target("turso_core::vdbe"));
    }

    #[test]
    fn level_override_round_trips() {
        assert_eq!(effective_level(), auto_level());
        set_level(Some(LevelFilter::Warn));
        assert_eq!(effective_level(), LevelFilter::Warn);
        assert!(level_is_overridden());
        set_level(None);
        assert_eq!(effective_level(), auto_level());
        assert!(!level_is_overridden());
    }

    fn rendered(colour: bool, thread_no: u16, args: std::fmt::Arguments<'_>) -> String {
        let mut line = FaultLine::new();
        render_fault(
            &mut line,
            colour,
            1.5,
            thread_no,
            "dtb_ke_crash::linux",
            args,
        );
        String::from_utf8(line.finish().to_vec()).unwrap()
    }

    #[test]
    fn fault_line_keeps_the_house_format() {
        let out = rendered(false, 0, format_args!("EXC_BAD_ACCESS at {:#x}", 0xdeadu64));
        assert_eq!(
            out,
            "[      1.500000] [F] [Main Thread] [dtb_ke_crash::linux] EXC_BAD_ACCESS at 0xdead\n"
        );
        // and it round-trips through the parser
        let parsed = parse_line(out.trim_end()).expect("fault line parses");
        assert_eq!(parsed.level_text, "F");
        assert_eq!(parsed.level, None);
        assert_eq!(parsed.thread, "Main Thread");
        assert_eq!(parsed.subsystem, "dtb_ke_crash::linux");
        assert_eq!(parsed.message, "EXC_BAD_ACCESS at 0xdead");
    }

    #[test]
    fn fault_line_thread_variants() {
        assert!(rendered(false, 7, format_args!("x")).contains("[DTB-KE-T 07]"));
        assert!(rendered(false, u16::MAX, format_args!("x")).contains("[?]"));
    }

    #[test]
    fn fault_line_colour_wraps_f_and_message_in_red() {
        let out = rendered(true, 0, format_args!("boom"));
        assert!(out.contains("[\x1b[31mF\x1b[0m]"), "F not red: {out:?}");
        assert!(
            out.contains("\x1b[31mboom\x1b[0m\n"),
            "message not red: {out:?}"
        );
        // timecode is dimmed (inside the brackets)
        assert!(
            out.starts_with("[\x1b[90m      1.500000\x1b[0m]"),
            "timecode not dimmed: {out:?}"
        );
    }

    #[test]
    fn fault_line_truncates_and_still_ends_in_one_newline() {
        let huge = "z".repeat(4000);
        let out = rendered(false, 0, format_args!("{huge}"));
        assert_eq!(out.len(), FaultLine::CAP);
        assert_eq!(out.matches('\n').count(), 1);
        assert!(out.ends_with('\n'));
    }

    /// End-to-end: `fault!` → the session file's raw descriptor. This is the
    /// only test that installs the process-global logger.
    #[test]
    fn fault_macro_writes_to_the_session_file() {
        let path =
            std::env::temp_dir().join(format!("dtb-ke-log-fault-{}.log", std::process::id()));
        let _ = std::fs::remove_file(&path);
        DTB_KE_GLOBAL_LOGGER
            .get_or_init(|| DTBKELogger::new(&path).expect("temp log file for the fault test"));

        fault!("deliberate {} fault #{:#x}", "test", 0x2a);

        let contents = std::fs::read_to_string(&path).expect("read the fault line back");
        let _ = std::fs::remove_file(&path);
        let line = contents.lines().next_back().expect("at least one line");
        let parsed = parse_line(line).expect("the fault line parses");
        assert_eq!(parsed.level_text, "F");
        assert_eq!(parsed.subsystem, "dtb_ke_log::tests");
        assert_eq!(parsed.message, "deliberate test fault #0x2a");
    }
}
