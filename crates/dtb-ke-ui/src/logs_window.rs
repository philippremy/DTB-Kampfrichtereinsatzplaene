//! The log-viewer window.
//!
//! A sidebar lists every `*.log` file in the log directory (newest first, with
//! size + modified date); selecting one shows it in the detail pane. Three
//! controls: **Neu laden** (re-read from disk), **Kopieren** (contents to the
//! clipboard) and **Einfärben** (a toggle — off by default — that colourises the
//! lines by level, matching `dtb_ke_log`'s stderr scheme).
//!
//! The body is a `uniform_list` — the whole file is read and split into line
//! ranges, but only the visible rows are ever laid out, so a multi-MB log opens
//! instantly and scrolls (both axes) smoothly.

use std::fs;
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use gpui_kit::{
    App, AppContext, Bounds, ClipboardItem, Context, FocusHandle, Focusable,
    HighlightStyle, InteractiveElement, IntoElement, ListHorizontalSizingBehavior, ParentElement,
    Render, ScrollHandle, ScrollStrategy, SharedString, Size, StatefulInteractiveElement, Styled,
    StyledText, TitlebarOptions, UniformListScrollHandle, Window, WindowBounds, WindowKind,
    WindowOptions, div, prelude::FluentBuilder, px, uniform_list,
};
use gpui_kit::base::Scrollbar;

use crate::window_registry::WindowRegistry;
use crate::components::icon::Icon;
use crate::components::toggle::Toggle;
use crate::components::{Button, ButtonTone};
use crate::filesystem::FilesystemHelper;
use crate::i18n::ActiveLocale;
use crate::theme::{ActiveTheme, PaletteColors};

/// A sanity ceiling — a file larger than this is not loaded at all (it would be
/// gigabytes of RAM); the user is told to open it externally.
const HARD_MAX_BYTES: u64 = 256 * 1024 * 1024;

static OPEN: WindowRegistry<()> = WindowRegistry::new();

/// Open (or focus) the log-viewer window.
pub fn open(cx: &mut App) {
    if crate::sheet::enabled() {
        let options = window_options(cx);
        crate::sheet::present(cx, "logs", &options, |_, cx| cx.new(LogsWindow::new));
        return;
    }
    // Existence via `cx.windows()`, not `handle.update` — see `about::open_kind`.
    if OPEN.focus((), cx) {
        return;
    }

    let options = window_options(cx);
    match cx.open_window(options, |_, cx| cx.new(LogsWindow::new)) {
        Ok(handle) => OPEN.insert((), handle.into()),
        Err(err) => log::error!("failed to open the logs window: {err}"),
    }
}

fn window_options(cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(960.), px(640.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(cx.t("logs.window-title")),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_minimizable: true,
        window_min_size: Some(Size::new(px(680.), px(420.))),
        ..Default::default()
    }
}

struct LogFile {
    path: PathBuf,
    name: String,
    size: u64,
    modified: Option<SystemTime>,
    is_current: bool,
}

/// A loaded log: the whole file (lossy-UTF-8), the byte range of every line
/// (newline excluded), and the index of the widest line (`uniform_list` measures
/// it to size the horizontal scroll extent).
struct LoadedLog {
    text: Arc<str>,
    lines: Arc<[Range<usize>]>,
    widest: usize,
}

enum Content {
    None,
    Error(String),
    Loaded(LoadedLog),
}

pub struct LogsWindow {
    focus: FocusHandle,
    files: Vec<LogFile>,
    selected: Option<usize>,
    content: Content,
    colorize: bool,
    list_scroll: ScrollHandle,
    body_scroll: UniformListScrollHandle,
}

impl Drop for LogsWindow {
    fn drop(&mut self) {
        OPEN.remove(());
    }
}

impl LogsWindow {
    fn new(cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            focus: cx.focus_handle(),
            files: Vec::new(),
            selected: None,
            content: Content::None,
            colorize: false,
            list_scroll: ScrollHandle::new(),
            body_scroll: UniformListScrollHandle::new(),
        };
        this.rescan();
        this.select(0, cx);
        this
    }

    /// Re-read the directory listing (keeps the current selection by path).
    fn rescan(&mut self) {
        let current = FilesystemHelper::instance().get_log_file();
        let dir = FilesystemHelper::instance().get_log_dir();
        let selected_path = self
            .selected
            .and_then(|ix| self.files.get(ix))
            .map(|f| f.path.clone());

        let mut files: Vec<LogFile> = fs::read_dir(dir)
            .into_iter()
            .flatten()
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) != Some("log") {
                    return None;
                }
                let meta = entry.metadata().ok();
                Some(LogFile {
                    name: path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    size: meta.as_ref().map(|m| m.len()).unwrap_or(0),
                    modified: meta.as_ref().and_then(|m| m.modified().ok()),
                    is_current: path == current,
                    path,
                })
            })
            .collect();

        files.sort_by(|a, b| {
            b.modified
                .cmp(&a.modified)
                .then_with(|| b.name.cmp(&a.name))
        });

        self.selected = selected_path
            .and_then(|path| files.iter().position(|f| f.path == path))
            .or(if files.is_empty() { None } else { Some(0) });
        self.files = files;
    }

    fn select(&mut self, index: usize, cx: &mut Context<Self>) {
        if index >= self.files.len() {
            return;
        }
        self.selected = Some(index);
        self.load(true, cx);
    }

    /// Re-read the selected file from disk. `reset_scroll` jumps back to the top
    /// (a fresh selection); the reload button keeps the position.
    fn load(&mut self, reset_scroll: bool, cx: &mut Context<Self>) {
        self.content = match self.selected.and_then(|ix| self.files.get(ix)) {
            None => Content::None,
            Some(file) => match read_log(&file.path) {
                Ok(loaded) => {
                    log::debug!("log viewer: loaded {}", file.path.display());
                    Content::Loaded(loaded)
                }
                Err(err) => {
                    let message = err.translate(cx);
                    log::warn!(
                        "log viewer: could not read {}: {message}",
                        file.path.display()
                    );
                    Content::Error(message)
                }
            },
        };
        if reset_scroll {
            self.body_scroll.scroll_to_item(0, ScrollStrategy::Top);
        }
        cx.notify();
    }

    fn copy(&self, cx: &mut App) {
        if let Content::Loaded(loaded) = &self.content {
            cx.write_to_clipboard(ClipboardItem::new_string(loaded.text.to_string()));
        }
    }

    // ── render ──────────────────────────────────────────────────────────

    fn sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        div()
            .flex()
            .flex_none()
            .flex_col()
            .w(px(248.))
            .h_full()
            .border_r_1()
            .border_color(c.border)
            .bg(c.chrome)
            .child(
                div()
                    .flex_none()
                    .px(px(14.))
                    .h(px(30.))
                    .flex()
                    .items_center()
                    .text_size(px(11.))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(c.muted_foreground)
                    .child(cx.t("logs.files-section")),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(Scrollbar::vertical(&self.list_scroll))
                    .child(
                        div()
                            .id("log-file-list")
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&self.list_scroll)
                            .px(px(6.))
                            .pb(px(8.))
                            .children(self.files.iter().enumerate().map(|(ix, file)| {
                                let selected = self.selected == Some(ix);
                                let meta = file_meta_line(file, cx);
                                div()
                                    .id(("log-file", ix))
                                    .flex()
                                    .flex_col()
                                    .gap(px(2.))
                                    .px(px(10.))
                                    .py(px(7.))
                                    .rounded(cx.theme().skin.radius_control_px())
                                    .when(selected, |el| {
                                        el.bg(gpui_kit::Hsla {
                                            a: 0.14,
                                            ..c.primary
                                        })
                                    })
                                    .when(!selected, |el| {
                                        el.cursor_pointer().hover(|el| {
                                            el.bg(gpui_kit::Hsla {
                                                a: 0.06,
                                                ..c.foreground
                                            })
                                        })
                                    })
                                    .child(
                                        div()
                                            .text_size(px(12.5))
                                            .text_color(if selected {
                                                c.primary
                                            } else {
                                                c.foreground
                                            })
                                            .child(file.name.clone()),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .text_color(c.muted_foreground)
                                            .child(meta),
                                    )
                                    .on_click(
                                        cx.listener(move |this, _, _window, cx| {
                                            this.select(ix, cx)
                                        }),
                                    )
                            })),
                    ),
            )
    }

    fn detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;

        div()
            .flex()
            .flex_1()
            .min_w(px(0.))
            .flex_col()
            .bg(c.background)
            .child(self.detail_toolbar(cx))
            .child(self.detail_body(&c, cx))
    }

    fn detail_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let has_file = self.selected.is_some();
        let loaded = matches!(self.content, Content::Loaded(_));

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .h(px(44.))
            .border_b_1()
            .border_color(c.border)
            .child(
                Button::new("log-reload", cx.t("logs.reload-button"))
                    .small()
                    .tone(ButtonTone::Secondary)
                    .disabled(!has_file)
                    .on_click(cx.listener(|this, _, _window, cx| {
                        this.rescan();
                        this.load(false, cx);
                    })),
            )
            .child(
                Button::new("log-copy", cx.t("logs.copy-button"))
                    .small()
                    .tone(ButtonTone::Secondary)
                    .leading_icon(Icon::Copy)
                    .disabled(!loaded)
                    .on_click(cx.listener(|this, _, _window, cx| this.copy(cx))),
            )
            .child(div().flex_1())
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(cx.t("logs.colorize-toggle")),
            )
            .child(
                Toggle::new("log-colorize", self.colorize).on_change(cx.processor(
                    |this, next: bool, _window, cx| {
                        this.colorize = next;
                        cx.notify();
                    },
                )),
            )
    }

    fn detail_body(&self, c: &PaletteColors, cx: &Context<Self>) -> gpui_kit::AnyElement {
        match &self.content {
            Content::None => body_frame(placeholder(cx.t("logs.no-file-selected"), c)),
            Content::Error(err) => body_frame(placeholder(err.clone(), c)),
            Content::Loaded(loaded) => {
                let text = loaded.text.clone();
                let lines = loaded.lines.clone();
                let widest = loaded.widest;
                let colorize = self.colorize;
                let colors = *c;
                // The skin's monospace face (Menlo / Consolas, or the generic name where the platform
                // resolves one), not a per-OS guess — CoreText has no "monospace" alias.
                let mono = cx.theme().skin.mono_font_family();

                let list = uniform_list("log-lines", lines.len(), move |range, _window, _cx| {
                    range
                        .filter_map(|ix| lines.get(ix).cloned())
                        .map(|span| log_line_el(&text[span], colorize, &colors))
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.body_scroll)
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(Some(widest))
                .size_full()
                .px(px(14.))
                .py(px(10.))
                .font_family(mono.clone())
                .text_size(px(11.5))
                .text_color(c.foreground);

                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(list)
                    .child(Scrollbar::vertical(&self.body_scroll))
                    .into_any_element()
            }
        }
    }
}

/// Wrap a small non-list body so it fills the pane.
fn body_frame(child: gpui_kit::AnyElement) -> gpui_kit::AnyElement {
    div().flex_1().min_h(px(0.)).child(child).into_any_element()
}

impl Render for LogsWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let font = cx.theme().skin.font_family();
        let lead = crate::skin::titlebar::content_leading_inset(window);

        if window.is_window_active() && window.focused(cx).is_none() {
            let handle = self.focus.clone();
            window.focus(&handle, cx);
        }

        div()
            .track_focus(&self.focus)
            .key_context("LogsWindow")
            .size_full()
            .relative()
            .children(crate::crash_countdown::overlay(window, cx, crate::crash_countdown::Mode::Scrim))
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .when_some(font, |el, family| el.font_family(family))
            .when(!crate::sheet::enabled(), |el| el.child(
                div()
                    .flex_none()
                    .h(px(34.))
                    .pl(lead)
                    .pr(px(16.))
                    .flex()
                    .items_center()
                    .border_b_1()
                    .border_color(c.border)
                    .bg(c.chrome)
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(cx.t("logs.window-title")),
            ))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.sidebar(cx))
                    .child(self.detail(cx)),
            )
    }
}

impl Focusable for LogsWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}

// ── helpers ─────────────────────────────────────────────────────────────

fn placeholder(text: impl Into<SharedString>, c: &PaletteColors) -> gpui_kit::AnyElement {
    div()
        .p(px(24.))
        .text_size(px(13.))
        .text_color(c.muted_foreground)
        .child(text.into())
        .into_any_element()
}

/// One log line as a `uniform_list` row — plain, or with per-field colour
/// highlights matching `dtb_ke_log`'s stderr scheme (timecode / thread /
/// subsystem dimmed, level letter coloured, an error / warning message tinted).
fn log_line_el(line: &str, colorize: bool, c: &PaletteColors) -> gpui_kit::AnyElement {
    let row = div().whitespace_nowrap().min_h(px(15.));
    let owned = SharedString::from(line.to_string());

    let Some(parsed) = colorize.then(|| dtb_ke_log::parse_line(line)).flatten() else {
        return row.child(owned).into_any_element();
    };

    let dim = HighlightStyle {
        color: Some(c.muted_foreground),
        ..Default::default()
    };
    let tint = |hsla| HighlightStyle {
        color: Some(hsla),
        ..Default::default()
    };
    // `[F]` (fault — from the crash handlers) has no `log::Level`; treat it like
    // an error but a touch hotter.
    let is_fault = parsed.level_text.trim() == "F";
    let level_color = match parsed.level {
        Some(log::Level::Error) => c.critical,
        Some(log::Level::Warn) => c.warn,
        Some(log::Level::Info) => c.ok,
        Some(log::Level::Debug) => c.primary,
        Some(log::Level::Trace) => c.muted_foreground,
        None if is_fault => c.critical,
        None => c.foreground,
    };

    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    let mut push = |slice: &str, style: HighlightStyle| {
        if let Some(start) = byte_offset(line, slice) {
            highlights.push((start..start + slice.len(), style));
        }
    };
    push(parsed.timecode, dim);
    push(parsed.level_text, tint(level_color));
    push(parsed.thread, dim);
    push(parsed.subsystem, dim);
    if is_fault || matches!(parsed.level, Some(log::Level::Error | log::Level::Warn)) {
        push(parsed.message, tint(level_color));
    }

    row.child(StyledText::new(owned).with_highlights(highlights))
        .into_any_element()
}

/// Byte offset of `inner` within `outer`, if `inner` is a sub-slice of it.
fn byte_offset(outer: &str, inner: &str) -> Option<usize> {
    let (o, i) = (outer.as_ptr() as usize, inner.as_ptr() as usize);
    (i >= o && i + inner.len() <= o + outer.len()).then_some(i - o)
}

/// Read the whole file (lossy UTF-8) and index its lines.
enum ReadLogError {
    TooLarge(u64),
    Io(String),
}

impl ReadLogError {
    fn translate(&self, cx: &App) -> String {
        match self {
            Self::TooLarge(len) => cx.t_fmt("logs.file-too-large", &[("size", &human_size(*len))]),
            Self::Io(detail) => detail.clone(),
        }
    }
}

fn read_log(path: &Path) -> Result<LoadedLog, ReadLogError> {
    let len = fs::metadata(path)
        .map_err(|e| ReadLogError::Io(e.to_string()))?
        .len();
    if len > HARD_MAX_BYTES {
        return Err(ReadLogError::TooLarge(len));
    }

    let bytes = fs::read(path).map_err(|e| ReadLogError::Io(e.to_string()))?;
    let text: Arc<str> = match String::from_utf8(bytes) {
        Ok(s) => Arc::from(s),
        Err(err) => Arc::from(String::from_utf8_lossy(err.as_bytes()).into_owned()),
    };

    let b = text.as_bytes();
    let mut lines: Vec<Range<usize>> = Vec::new();
    let mut start = 0usize;
    for (i, byte) in b.iter().enumerate() {
        if *byte == b'\n' {
            let end = if i > start && b[i - 1] == b'\r' {
                i - 1
            } else {
                i
            };
            lines.push(start..end);
            start = i + 1;
        }
    }
    if start < b.len() {
        lines.push(start..b.len());
    }
    if lines.is_empty() {
        lines.push(0..0);
    }

    let widest = lines
        .iter()
        .enumerate()
        .max_by_key(|(_, span)| text[(*span).clone()].chars().count())
        .map(|(ix, _)| ix)
        .unwrap_or(0);

    Ok(LoadedLog {
        text,
        lines: Arc::from(lines),
        widest,
    })
}

fn file_meta_line(file: &LogFile, cx: &Context<LogsWindow>) -> String {
    let mut parts = vec![human_size(file.size)];
    if let Some(modified) = file.modified {
        let dt: chrono::DateTime<chrono::Local> =
            chrono::DateTime::<chrono::Utc>::from(modified).with_timezone(&chrono::Local);
        parts.push(dt.format("%d.%m.%Y %H:%M").to_string());
    }
    if file.is_current {
        parts.push(cx.t("logs.current-session").to_string());
    }
    parts.join(" · ")
}

fn human_size(bytes: u64) -> String {
    const UNITS: [&str; 4] = ["B", "KB", "MB", "GB"];
    let mut size = bytes as f64;
    let mut unit = 0;
    while size >= 1024.0 && unit < UNITS.len() - 1 {
        size /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{size:.1} {}", UNITS[unit]).replace('.', ",")
    }
}
