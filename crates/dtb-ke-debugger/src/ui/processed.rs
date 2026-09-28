//! "Processed": process facts, the thread list, the backtrace table
//! (`Frame | Trust | Module | Signature | Source`), the selected frame's registers and its source.

use std::sync::Arc;

use dtb_ke_debugger::code::{self, SourceText};
use dtb_ke_debugger::progress::Progress;
use dtb_ke_debugger::sources::{self, Plan};
use dtb_ke_ui::components::icon::Icon;
use dtb_ke_ui::components::{Button, ButtonTone};
use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::base::Scrollbar;
use gpui_kit::base::{ResizableState, SelectableText, h_resizable, resizable_panel, v_resizable};
use gpui_kit::{
    AnyElement, AppContext, Context, DispatchPhase, DragMoveEvent, HighlightStyle,
    InteractiveElement, IntoElement, ListHorizontalSizingBehavior, MouseButton, MouseDownEvent,
    OngoingScroll, ParentElement, Pixels, Render, ScrollDelta, ScrollHandle, ScrollStrategy,
    ScrollWheelEvent, SharedString, StatefulInteractiveElement, Styled, StyledText,
    UniformListScrollHandle, Window, canvas, div, point, prelude::FluentBuilder, px, uniform_list,
};
use minidump::Module;
use minidump_processor::ProcessState;
use minidump_unwind::{FrameTrust, StackFrame};

use super::DebuggerWindow;
use super::widgets::{
    self, ListStyle, TextBlock, listing, listing_rows, section_title, selectable, tint,
};
use crate::tokio_bridge::Tokio;
use dtb_ke_debugger::highlight::{Highlighted, Token, highlight};

/// One line of the backtrace: a real frame, or an inlined frame beneath it.
#[derive(Clone)]
pub struct Row {
    /// The real frame this row belongs to (registers come from it).
    pub frame: usize,
    pub inline: bool,
    pub num: String,
    pub trust: &'static str,
    pub module: String,
    pub source: String,
    pub signature: String,
    pub file: Option<String>,
    pub line: Option<u32>,
}

#[derive(Default)]
pub enum CodeView {
    #[default]
    None,
    /// Not on this machine. `plan` says whether (and from where) it could be fetched — on the user's say-so.
    Missing {
        file: String,
        line: u32,
        plan: Result<Plan, String>,
    },
    /// A fetch the user asked for is running.
    #[allow(unused)]
    Fetching {
        file: String,
        line: u32,
        plan: Plan,
        /// Steps / bytes of the running fetch, for its bar.
        progress: Arc<Progress>,
    },
    FetchFailed {
        file: String,
        line: u32,
        plan: Plan,
        error: String,
    },
    Loaded {
        text: SourceText,
        block: TextBlock,
        /// Tokens per line, when there is a grammar for the file.
        highlights: Option<Arc<Highlighted>>,
        line: u32,
    },
}

#[derive(Default)]
pub struct State {
    pub thread: usize,
    pub selected: usize,
    pub rows: Arc<[Row]>,
    pub code: CodeView,
    rows_scroll: UniformListScrollHandle,
    code_scroll: UniformListScrollHandle,
    /// The one scroll area of the sidebar (process facts + thread list).
    sidebar_scroll: ScrollHandle,
    regs_scroll: ScrollHandle,
    /// Backtrace column widths (px) — Module / Signature / Source are user-resizable. The last column
    /// (Source) is a *minimum*: it grows to fill a wider window.
    cols: Cols,
    /// Where the pointer was at the last column-drag event (for the delta).
    drag_x: Option<Pixels>,
    table_scroll: ScrollHandle,
    /// Axis lock of the current trackpad gesture (see `axis_locked_wheel`).
    ongoing: std::rc::Rc<std::cell::RefCell<OngoingScroll>>,
    /// Source pane / register pane visibility (both hidden → the backtrace fills the view).
    pub code_hidden: bool,
    pub regs_hidden: bool,
    /// Remembered sizes of the two resizable panes (`0` = not set yet → the defaults below).
    bottom_height: f32,
    regs_width: f32,
    v_state: Option<gpui_kit::Entity<ResizableState>>,
    h_state: Option<gpui_kit::Entity<ResizableState>>,
}

const BOTTOM_HEIGHT: f32 = 300.;
const BOTTOM_MIN: f32 = 110.;
const REGS_WIDTH: f32 = 300.;
const REGS_MIN: f32 = 180.;
const REGS_MAX: f32 = 640.;
/// Releasing the register divider narrower than this hides the register pane.
const REGS_SNAP: f32 = 210.;

#[derive(Clone, Copy)]
struct Cols {
    module: f32,
    source: f32,
    signature: f32,
}

impl Default for Cols {
    fn default() -> Self {
        Self {
            module: 150.,
            source: 300.,
            signature: 520.,
        }
    }
}

/// Which backtrace column a header drag handle resizes.
#[derive(Clone, Copy)]
enum Col {
    Module,
    Source,
    Signature,
}

/// Drag payload of a column handle.
#[derive(Clone, Copy)]
struct ColDrag(Col);

/// The invisible "ghost" gpui draws under the cursor while dragging a handle.
struct NoGhost;

impl Render for NoGhost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
    }
}

const COL_MIN: f32 = 60.;
const COL_FRAME: f32 = 40.;
const COL_TRUST: f32 = 96.;
const COL_GAP: f32 = 10.;
const ROW_PAD: f32 = 14.;
/// Average glyph width of the 11.5 px monospace face, for sizing columns to their content.
const CHAR_W: f32 = 6.9;

/// Columns wide enough for the longest content (clamped), so nothing is cut off by default and
/// the table scrolls horizontally instead.
fn auto_cols(rows: &[Row]) -> Cols {
    let widest = |f: fn(&Row) -> usize, lo: f32, hi: f32| {
        let n = rows.iter().map(f).max().unwrap_or(0) as f32;
        (n * CHAR_W + 12.).clamp(lo, hi)
    };
    Cols {
        module: widest(|r| r.module.chars().count(), 100., 260.),
        source: widest(|r| r.source.chars().count(), 160., 640.),
        signature: widest(|r| r.signature.chars().count(), 320., 1600.),
    }
}

/// A fetched file as a [`SourceText`]: keyed by the debug-info path (so the same file is recognised across frames).
fn fetched_text(file: &str, plan: &Plan, text: String) -> SourceText {
    SourceText {
        path: std::path::PathBuf::from(file),
        origin: format!("fetched from {}", plan.source),
        text: Arc::from(text),
    }
}

fn trust_label(t: FrameTrust) -> &'static str {
    match t {
        FrameTrust::None => "none",
        FrameTrust::Scan => "Scan",
        FrameTrust::CfiScan => "CFI-Scan",
        FrameTrust::FramePointer => "Frame Pointer",
        FrameTrust::CallFrameInfo => "CFI",
        FrameTrust::PreWalked => "prewalked",
        FrameTrust::Context => "context",
    }
}

fn basename(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// `file` relative to the build's workspace root when it is inside it — the part worth reading.
fn short_path<'a>(file: &'a str, root: Option<&str>) -> &'a str {
    match root.and_then(|r| file.strip_prefix(r)) {
        Some(rest) => rest.trim_start_matches(['/', '\\']),
        None => file,
    }
}

fn signature(f: &StackFrame, module: &str) -> String {
    match (&f.function_name, f.function_base) {
        (Some(name), Some(base)) if f.instruction > base => {
            format!("{name} + {:#x}", f.instruction - base)
        }
        (Some(name), _) => name.clone(),
        (None, _) => match &f.module {
            Some(m) => format!(
                "{module} + {:#x}",
                f.instruction.wrapping_sub(m.base_address())
            ),
            None => format!("{:#x}", f.instruction),
        },
    }
}

pub fn build_rows(
    state: &ProcessState,
    thread: usize,
    names: &std::collections::HashMap<u64, String>,
    root: Option<&str>,
) -> Vec<Row> {
    let Some(stack) = state.threads.get(thread) else {
        return Vec::new();
    };
    let mut rows = Vec::new();
    for (i, f) in stack.frames.iter().enumerate() {
        let module = f
            .module
            .as_ref()
            .map(|m| {
                names
                    .get(&m.base_address())
                    .cloned()
                    .unwrap_or_else(|| basename(&m.code_file()).to_owned())
            })
            .unwrap_or_default();
        let source_of = |file: Option<&String>, line: Option<u32>| match (file, line) {
            (Some(file), Some(line)) => format!("{}:{line}", short_path(file, root)),
            (Some(file), None) => short_path(file, root).to_owned(),
            _ => String::new(),
        };
        // Inline frames run innermost-first, so they come before the real frame that contains them.
        for inl in &f.inlines {
            rows.push(Row {
                frame: i,
                inline: true,
                num: String::new(),
                trust: "",
                module: module.clone(),
                source: source_of(inl.source_file_name.as_ref(), inl.source_line),
                signature: format!("(inline) {}", inl.function_name),
                file: inl.source_file_name.clone(),
                line: inl.source_line,
            });
        }
        rows.push(Row {
            frame: i,
            inline: false,
            num: i.to_string(),
            trust: trust_label(f.trust),
            source: source_of(f.source_file_name.as_ref(), f.source_line),
            signature: signature(f, &module),
            module,
            file: f.source_file_name.clone(),
            line: f.source_line,
        });
    }
    rows
}

impl DebuggerWindow {
    /// The analysis just landed: show the crashing thread, top frame selected.
    pub(super) fn processed_analysis_ready(&mut self, cx: &mut Context<Self>) {
        let Some(analysis) = self.analysis().cloned() else {
            return;
        };
        let thread = analysis
            .state
            .requesting_thread
            .unwrap_or(0)
            .min(analysis.state.threads.len().saturating_sub(1));
        self.select_thread(thread, cx);
    }

    fn select_thread(&mut self, thread: usize, cx: &mut Context<Self>) {
        let (Some(analysis), Some(session)) = (self.analysis().cloned(), self.session.as_ref())
        else {
            return;
        };
        let root = self.source_roots.build_root.clone();
        let rows = build_rows(&analysis.state, thread, &session.names, root.as_deref());
        // Land on the first frame that has source, so the code view is useful immediately.
        let selected = rows
            .iter()
            .position(|r| r.file.is_some() && r.line.is_some())
            .unwrap_or(0);
        self.processed.thread = thread;
        self.processed.cols = auto_cols(&rows);
        self.processed.rows = Arc::from(rows);
        self.select_row(selected, cx);
    }

    fn select_row(&mut self, ix: usize, cx: &mut Context<Self>) {
        self.processed.selected = ix;
        let target = match self.processed.rows.get(ix) {
            Some(Row {
                file: Some(file),
                line: Some(line),
                ..
            }) => Some((file.clone(), *line)),
            _ => None,
        };
        self.processed.code = match target {
            Some((file, line)) => self.source_for(&file, line),
            None => CodeView::None,
        };
        self.processed
            .rows_scroll
            .scroll_to_item(ix, ScrollStrategy::Nearest);
        // Debug aid: `DTB_KE_AUTO_FETCH=1` acts as if the user had clicked "Fetch source".
        if std::env::var_os("DTB_KE_AUTO_FETCH").is_some() {
            self.fetch_source(cx);
        }
        cx.notify();
    }

    /// The source pane's content for `file:line`: a local copy if there is one, else one fetched earlier (already on
    /// disk — no permission needed to read that), else a prompt to fetch it.
    fn source_for(&mut self, file: &str, line: u32) -> CodeView {
        // Debug aid: `DTB_KE_NO_LOCAL_SOURCES=1` pretends no source is on this machine, to exercise fetching.
        if std::env::var_os("DTB_KE_NO_LOCAL_SOURCES").is_none() {
            if let Some(text) = code::load(file, &self.source_roots) {
                return self.loaded(text, file, line);
            }
        }
        let build = self.session.as_ref().and_then(|s| s.opened.build.as_ref());
        let plan = sources::plan(file, build);
        log::debug!(
            "no local source for {file}: fetch plan {:?}",
            plan.as_ref().map(|p| (&p.title, &p.source))
        );
        if let Ok(p) = &plan {
            if let Ok(text) = sources::fetch_blocking(p, &self.sources_cache, false) {
                return self.loaded(fetched_text(file, p, text), file, line);
            }
        }
        CodeView::Missing {
            file: file.to_owned(),
            line,
            plan,
        }
    }

    fn loaded(&self, text: SourceText, file: &str, line: u32) -> CodeView {
        // Same file as the one already shown (another frame in it): keep its split lines and highlighting instead of
        // parsing the whole file again.
        let (block, highlights) = match &self.processed.code {
            CodeView::Loaded {
                text: shown,
                block,
                highlights,
                ..
            } if shown.path == text.path => (block.clone(), highlights.clone()),
            _ => (
                TextBlock::new(text.text.to_string()),
                highlight(file, &text.text).map(Arc::new),
            ),
        };
        self.processed
            .code_scroll
            .scroll_to_item(line.saturating_sub(1) as usize, ScrollStrategy::Center);
        CodeView::Loaded {
            text,
            block,
            highlights,
            line,
        }
    }

    /// The user agreed to download the missing file: fetch just that file (partial git request / one crate file) off
    /// the UI thread, then show it.
    pub(super) fn fetch_source(&mut self, cx: &mut Context<Self>) {
        let (file, line, plan) = match &self.processed.code {
            CodeView::Missing {
                file,
                line,
                plan: Ok(plan),
            }
            | CodeView::FetchFailed {
                file, line, plan, ..
            } => (file.clone(), *line, plan.clone()),
            _ => return,
        };
        let progress = Progress::new();
        self.processed.code = CodeView::Fetching {
            file: file.clone(),
            line,
            plan: plan.clone(),
            progress: progress.clone(),
        };
        log::info!("fetching {} from {}", plan.title, plan.source);

        let (cache, worker_plan) = (self.sources_cache.clone(), plan.clone());
        let worker_progress = progress.clone();
        let work = Tokio::spawn_result(cx, async move {
            Ok(tokio::task::spawn_blocking(move || {
                sources::fetch_with_progress(&worker_plan, &cache, true, &worker_progress)
            })
            .await??)
        });
        // Repaint while the fetch runs, so its bar moves.
        let ticking = file.clone();
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(std::time::Duration::from_millis(120)).await;
                let still_fetching = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        matches!(&this.processed.code, CodeView::Fetching { file, .. } if *file == ticking)
                    })
                    .unwrap_or(false);
                if !still_fetching {
                    break;
                }
            }
        }));
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.source_fetched(file, line, plan, result, cx)
            })
            .ok();
        }));
        cx.notify();
    }

    fn source_fetched(
        &mut self,
        file: String,
        line: u32,
        plan: Plan,
        result: anyhow::Result<String>,
        cx: &mut Context<Self>,
    ) {
        // The user may have selected another frame meanwhile; the file is cached either way, so just drop the result.
        if !matches!(&self.processed.code, CodeView::Fetching { file: f, .. } if *f == file) {
            return;
        }
        self.processed.code = match result {
            Ok(text) => {
                log::info!("fetched {} ({} bytes)", plan.title, text.len());
                self.loaded(fetched_text(&file, &plan, text), &file, line)
            }
            Err(err) => {
                log::warn!("fetching {} failed: {err:#}", plan.title);
                CodeView::FetchFailed {
                    file,
                    line,
                    plan,
                    error: format!("{err:#}"),
                }
            }
        };
        cx.notify();
    }

    pub(super) fn render_processed(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let c = cx.theme().color;
        let Some(analysis) = self.analysis().cloned() else {
            return div().into_any_element();
        };
        let state = &analysis.state;

        // One scroll area for the whole sidebar: the process facts and the thread list scroll together.
        let left = div()
            .relative()
            .size_full()
            .bg(c.chrome)
            .child(
                div()
                    .id("sidebar-scroll")
                    .size_full()
                    .overflow_y_scroll()
                    .track_scroll(&self.processed.sidebar_scroll)
                    .child(section_title("Process", &c))
                    .child(listing_rows(
                        self.process_rows(state),
                        ListStyle::Stacked,
                        cx,
                    ))
                    .child(section_title(
                        format!("Threads ({})", state.threads.len()),
                        &c,
                    ))
                    .child(self.thread_list(state, cx)),
            )
            .child(Scrollbar::vertical(&self.processed.sidebar_scroll));

        let right = self.right_column(state, cx);

        self.with_sidebar(left.into_any_element(), right.into_any_element(), cx)
    }

    pub(super) fn toggle_code(&mut self, cx: &mut Context<Self>) {
        self.processed.code_hidden = !self.processed.code_hidden;
        self.reset_bottom_layout(cx);
    }

    pub(super) fn toggle_registers(&mut self, cx: &mut Context<Self>) {
        self.processed.regs_hidden = !self.processed.regs_hidden;
        self.reset_bottom_layout(cx);
    }

    /// The set of visible panes changed: let both splits lay out afresh at the remembered sizes.
    fn reset_bottom_layout(&mut self, cx: &mut Context<Self>) {
        for state in [&self.processed.v_state, &self.processed.h_state]
            .into_iter()
            .flatten()
        {
            state.update(cx, |s, _| s.clear());
        }
        cx.notify();
    }

    /// Backtrace on top; below it source and registers side by side. Both dividers drag; either pane can be
    /// hidden (toolbar / View menu), the register pane also by dragging it narrow (like the sidebar).
    fn right_column(&mut self, state: &ProcessState, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.theme().color;
        let show_code = !self.processed.code_hidden;
        let show_regs = !self.processed.regs_hidden;
        let backtrace = self.backtrace(cx).into_any_element();
        if !show_code && !show_regs {
            return div()
                .flex()
                .flex_col()
                .size_full()
                .min_w(px(0.))
                .child(backtrace)
                .into_any_element();
        }

        if self.processed.v_state.is_none() {
            self.processed.v_state = Some(cx.new(|_| ResizableState::default()));
            self.processed.h_state = Some(cx.new(|_| ResizableState::default()));
        }
        let (v_state, h_state) = (
            self.processed.v_state.clone().unwrap(),
            self.processed.h_state.clone().unwrap(),
        );
        let bottom_h = if self.processed.bottom_height > 0. {
            self.processed.bottom_height
        } else {
            BOTTOM_HEIGHT
        };
        let regs_w = if self.processed.regs_width > 0. {
            self.processed.regs_width
        } else {
            REGS_WIDTH
        };

        let bottom: AnyElement = match (show_code, show_regs) {
            (true, true) => {
                let weak = cx.weak_entity();
                let code = self.code_view(cx).into_any_element();
                let regs = self.registers(state, cx).into_any_element();
                h_resizable("code-split")
                    .with_state(&h_state)
                    .on_resize(move |state, _window, cx| {
                        let width = state
                            .read(cx)
                            .sizes()
                            .get(1)
                            .copied()
                            .map(f32::from)
                            .unwrap_or(REGS_WIDTH);
                        weak.update(cx, |this, cx| {
                            if width < REGS_SNAP {
                                this.processed.regs_hidden = true;
                                this.reset_bottom_layout(cx);
                            } else {
                                this.processed.regs_width = width;
                            }
                            cx.notify();
                        })
                        .ok();
                    })
                    .child(resizable_panel().child(code))
                    .child(
                        resizable_panel()
                            .size(px(regs_w))
                            .size_range(px(REGS_MIN)..px(REGS_MAX))
                            .flex_none()
                            .child(regs),
                    )
                    .into_any_element()
            }
            (true, false) => self.code_view(cx).into_any_element(),
            (false, _) => self.registers(state, cx).into_any_element(),
        };

        let weak = cx.weak_entity();
        v_resizable("backtrace-split")
            .with_state(&v_state)
            .on_resize(move |state, _window, cx| {
                let height = state
                    .read(cx)
                    .sizes()
                    .get(1)
                    .copied()
                    .map(f32::from)
                    .unwrap_or(BOTTOM_HEIGHT);
                weak.update(cx, |this, cx| {
                    this.processed.bottom_height = height;
                    cx.notify();
                })
                .ok();
            })
            .child(
                resizable_panel().child(
                    div()
                        .flex()
                        .flex_col()
                        .size_full()
                        .min_h(px(0.))
                        .child(backtrace),
                ),
            )
            .child(
                resizable_panel()
                    .size(px(bottom_h))
                    .size_range(px(BOTTOM_MIN)..px(4000.))
                    .flex_none()
                    .child(
                        div()
                            .size_full()
                            .border_t_1()
                            .border_color(c.border)
                            .child(bottom),
                    ),
            )
            .into_any_element()
    }

    fn process_rows(&self, state: &ProcessState) -> Vec<(String, String)> {
        let mut rows = vec![
            ("OS".into(), state.system_info.os.to_string()),
            (
                "OS version".into(),
                state
                    .system_info
                    .format_os_version()
                    .map(|s| s.into_owned())
                    .unwrap_or_default(),
            ),
            ("CPU".into(), state.system_info.cpu.to_string()),
        ];
        if let Some(e) = &state.exception_info {
            rows.push(("Crash reason".into(), e.reason.to_string()));
            rows.push(("Crash address".into(), format!("{:#018x}", e.address.0)));
        }
        if let Some(ex) = self.session.as_ref().and_then(|s| s.opened.nsexception.as_ref()) {
            // The much more useful reason an uncaught `NSException` actually gives — the Mach
            // exception above it is just AppKit's own trap once it decided to abort.
            rows.push(("NSException".into(), ex.name.clone()));
            rows.push(("NSException reason".into(), ex.reason.clone()));
        }
        if let Some(a) = &state.assertion {
            rows.push(("Assertion".into(), a.clone()));
        }
        if let Some(t) = state.requesting_thread {
            rows.push(("Crashing thread".into(), thread_name(state, t)));
        }
        if let Some(b) = self.session.as_ref().and_then(|s| s.opened.build.as_ref()) {
            rows.push(("Version".into(), b.app_version().unwrap_or("?").into()));
            rows.push((
                "Commit".into(),
                b.commit()
                    .map(|c| c.chars().take(12).collect())
                    .unwrap_or_default(),
            ));
        }
        rows
    }

    fn thread_list(&self, state: &ProcessState, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        div()
            .px(px(6.))
            .pb(px(8.))
            .children((0..state.threads.len()).map(|ix| {
                let selected = self.processed.thread == ix;
                let crashed = state.requesting_thread == Some(ix);
                selectable(("thread", ix), selected, &c, radius)
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(6.))
                    .text_size(px(12.))
                    .text_color(if selected { c.primary } else { c.foreground })
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(0.))
                            .overflow_hidden()
                            .whitespace_nowrap()
                            .text_ellipsis()
                            .child(SharedString::from(thread_name(state, ix))),
                    )
                    .when(crashed, |el| {
                        el.child(
                            div()
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(c.critical)
                                .child("crashed"),
                        )
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.select_thread(ix, cx)))
            }))
    }

    fn resize_column(&mut self, col: Col, event: &DragMoveEvent<ColDrag>) {
        let x = event.event.position.x;
        let Some(last) = self.processed.drag_x.replace(x) else {
            return;
        };
        let delta = f32::from(x - last);
        let cols = &mut self.processed.cols;
        let width = match col {
            Col::Module => &mut cols.module,
            Col::Source => &mut cols.source,
            Col::Signature => &mut cols.signature,
        };
        *width = (*width + delta).max(COL_MIN);
    }

    /// A trackpad gesture rarely moves on one axis only, and the table scrolls on both (horizontally as a
    /// whole, vertically inside the list) — each scroller takes *its* axis of the same event, so a slightly
    /// slanted swipe drifts diagonally. gpui's `OngoingScroll` locks a gesture to its dominant axis (it is what
    /// `restrict_scroll_to_axis` uses inside one element; here it spans two). A capture-phase wheel listener
    /// runs it, applies the surviving axis to the right scroller itself, and stops the event so neither
    /// scroller sees it. A mouse wheel (line deltas) is left alone.
    fn axis_locked_wheel(&self) -> impl IntoElement {
        let horizontal = self.processed.table_scroll.clone();
        let vertical = self.processed.rows_scroll.clone();
        let ongoing = self.processed.ongoing.clone();
        canvas(
            |_, _, _| {},
            move |bounds, _, window, _cx| {
                let (horizontal, vertical, ongoing) =
                    (horizontal.clone(), vertical.clone(), ongoing.clone());
                window.on_mouse_event(move |event: &ScrollWheelEvent, phase, window, cx| {
                    if phase != DispatchPhase::Capture
                        || !matches!(event.delta, ScrollDelta::Pixels(_))
                        || !bounds.contains(&event.position)
                    {
                        return;
                    }
                    let mut delta = event.delta.pixel_delta(px(20.));
                    ongoing.borrow_mut().filter(&mut delta, event.touch_phase);

                    // Unlocked (a clear change of direction): take the larger axis, never both.
                    if delta.x.abs() > delta.y.abs() {
                        let (offset, max) = (horizontal.offset(), horizontal.max_offset());
                        let x = (offset.x + delta.x).max(-max.x).min(px(0.));
                        horizontal.set_offset(point(x, offset.y));
                    } else if delta.y != px(0.) {
                        let base = vertical.0.borrow().base_handle.clone();
                        let (offset, max) = (base.offset(), base.max_offset());
                        let mut y = (offset.y + delta.y).min(px(0.));
                        if max.y > px(0.) {
                            y = y.max(-max.y);
                        }
                        base.set_offset(point(offset.x, y));
                    }
                    cx.stop_propagation();
                    window.refresh();
                });
            },
        )
        // Explicit insets: an absolutely positioned child without them sits at its *flow* position (after its
        // siblings), which put this overlay over the code view instead of the table.
        .absolute()
        .top_0()
        .left_0()
        .size_full()
    }

    fn backtrace(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let mono = cx.theme().skin.mono_font_family();
        let rows = self.processed.rows.clone();
        let selected = self.processed.selected;
        let me = cx.entity();
        let cols = self.processed.cols;

        // The table is exactly as wide as its columns (so it scrolls horizontally when that exceeds the
        // pane) but never narrower than the pane; every row spans it, so the selection highlight does too.
        let fixed = ROW_PAD * 2.
            + COL_FRAME
            + COL_TRUST
            + cols.module
            + cols.source
            + cols.signature
            + COL_GAP * 4.;

        let header = |label: &'static str, col: Option<Col>, width: Option<f32>, fill: bool| {
            let cell = div()
                .relative()
                .text_size(px(11.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(c.muted_foreground)
                .child(label);
            let cell = match (fill, width) {
                (true, Some(w)) => cell.flex_1().min_w(px(w)),
                (_, Some(w)) => cell.flex_none().w(px(w)),
                _ => cell.flex_1(),
            };
            match col {
                Some(col) => cell.child(
                    div()
                        .id(label)
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .right(px(-COL_GAP / 2. - 3.))
                        .w(px(6.))
                        .cursor_col_resize()
                        .hover(move |el| el.bg(tint(c.primary, 0.5)))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(|this, ev: &MouseDownEvent, _, cx| {
                                this.processed.drag_x = Some(ev.position.x);
                                cx.stop_propagation();
                            }),
                        )
                        .on_drag(ColDrag(col), |_, _, _, cx| cx.new(|_| NoGhost)),
                ),
                None => cell,
            }
        };

        let list = uniform_list("backtrace", rows.len(), move |range, _w, _cx| {
            range
                .filter_map(|ix| rows.get(ix).cloned().map(|r| (ix, r)))
                .map(|(ix, r)| {
                    let me = me.clone();
                    let is_sel = ix == selected;
                    let order_base = ix as u64 * 5;
                    div()
                        .id(("bt", ix))
                        .flex()
                        .items_center()
                        .gap(px(COL_GAP))
                        .px(px(ROW_PAD))
                        .w_full()
                        .h(px(22.))
                        .text_size(px(11.5))
                        .font_family(mono.clone())
                        .when(is_sel, |el| el.bg(tint(c.primary, 0.16)))
                        .when(!is_sel, |el| {
                            el.cursor_pointer()
                                .hover(move |el| el.bg(tint(c.foreground, 0.06)))
                        })
                        .text_color(if r.inline {
                            c.muted_foreground
                        } else {
                            c.foreground
                        })
                        .child(
                            div()
                                .flex_none()
                                .w(px(COL_FRAME))
                                .text_color(c.muted_foreground)
                                .child(
                                    SelectableText::new(("bt-num", ix), r.num.clone())
                                        .document_order(order_base),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(COL_TRUST))
                                .text_color(c.muted_foreground)
                                .child(
                                    SelectableText::new(("bt-trust", ix), r.trust)
                                        .document_order(order_base + 1),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(cols.module))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(
                                    SelectableText::new(("bt-module", ix), r.module.clone())
                                        .document_order(order_base + 2),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .w(px(cols.signature))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .child(
                                    SelectableText::new(("bt-sig", ix), r.signature.clone())
                                        .document_order(order_base + 3),
                                ),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w(px(cols.source))
                                .overflow_hidden()
                                .whitespace_nowrap()
                                .text_ellipsis()
                                .text_color(c.primary)
                                .child(
                                    SelectableText::new(("bt-src", ix), r.source.clone())
                                        .document_order(order_base + 4),
                                ),
                        )
                        .on_click(move |_, _, cx| {
                            me.update(cx, |this, cx| this.select_row(ix, cx));
                        })
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.processed.rows_scroll)
        .size_full();
        // (`UniformList` is not a stateful element, so the flag goes straight onto its style.)
        let mut list = list;
        list.style().restrict_scroll_to_axis = Some(true);

        let table = div()
            .flex()
            .flex_col()
            .w(px(fixed))
            .min_w_full()
            .h_full()
            .on_drag_move(cx.listener(|this, ev: &DragMoveEvent<ColDrag>, _, cx| {
                let col = ev.drag(cx).0;
                this.resize_column(col, ev);
                cx.notify();
            }))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap(px(COL_GAP))
                    .px(px(ROW_PAD))
                    .w_full()
                    .h(px(28.))
                    .border_b_1()
                    .border_color(c.border)
                    .child(header("Frame", None, Some(COL_FRAME), false))
                    .child(header("Trust", None, Some(COL_TRUST), false))
                    .child(header(
                        "Module",
                        Some(Col::Module),
                        Some(cols.module),
                        false,
                    ))
                    .child(header(
                        "Signature",
                        Some(Col::Signature),
                        Some(cols.signature),
                        false,
                    ))
                    .child(header("Source", Some(Col::Source), Some(cols.source), true)),
            )
            .child(div().flex_1().min_h(px(0.)).child(list));

        // The vertical bar belongs to the pane (always visible at its right edge), the horizontal one to
        // the bottom edge; both sit outside the horizontally scrolling content.
        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(
                div()
                    .id("backtrace-scroll")
                    .size_full()
                    .overflow_x_scroll()
                    .restrict_scroll_to_axis()
                    .track_scroll(&self.processed.table_scroll)
                    .child(table),
            )
            .child(self.axis_locked_wheel())
            .child(Scrollbar::vertical(&self.processed.rows_scroll))
            .child(Scrollbar::horizontal(&self.processed.table_scroll))
    }

    fn code_view(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let mono = cx.theme().skin.mono_font_family();
        let title = match &self.processed.code {
            CodeView::Loaded { text, line, .. } => {
                format!(
                    "Source — {}:{line} ({})",
                    text.path
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    text.origin
                )
            }
            _ => "Source".to_owned(),
        };
        let body: AnyElement = match &self.processed.code {
            CodeView::None => widgets::placeholder("This frame has no source line.", &c),
            CodeView::Missing { file, line, plan } => match plan {
                Ok(plan) => fetch_prompt(plan, None, "Fetch source", &c, cx),
                Err(why) => widgets::placeholder(
                    format!(
                        "Source file not found: {file}:{line}. It lives on the build machine, and cannot be fetched: {why}."
                    ),
                    &c,
                ),
            },
            CodeView::Fetching { plan, progress, .. } => super::progress_view::fetch_card(
                format!("Fetching {} from {}", plan.title, plan.source),
                &progress.snapshot(),
                cx,
            ),
            CodeView::FetchFailed { plan, error, .. } => {
                fetch_prompt(plan, Some(error), "Try again", &c, cx)
            }
            CodeView::Loaded {
                block,
                highlights,
                line,
                ..
            } => {
                let (block, highlights, target) =
                    (block.clone(), highlights.clone(), *line as usize);
                let (count, widest) = (block.len(), block.widest());
                let list = uniform_list("code", count, move |range, _w, _cx| {
                    range
                        .map(|ix| {
                            let hit = ix + 1 == target;
                            // `min_w_full`: the row (and so the highlight) spans the list's whole content
                            // width — the widest line, or the viewport when that is narrower.
                            div()
                                .flex()
                                .whitespace_nowrap()
                                .min_w_full()
                                .min_h(px(16.))
                                .when(hit, |el| el.bg(tint(c.primary, 0.20)))
                                .child(
                                    div()
                                        .flex_none()
                                        .w(px(48.))
                                        .pr(px(10.))
                                        .text_right()
                                        .text_color(c.muted_foreground)
                                        .child(SharedString::from((ix + 1).to_string())),
                                )
                                .child(code_line(
                                    block.line(ix),
                                    highlights.as_deref().and_then(|h| h.lines.get(ix)),
                                    &c,
                                ))
                        })
                        .collect::<Vec<_>>()
                })
                .track_scroll(&self.processed.code_scroll)
                .with_horizontal_sizing_behavior(ListHorizontalSizingBehavior::Unconstrained)
                .with_width_from_item(Some(widest))
                .size_full()
                .py(px(6.))
                .font_family(mono)
                .text_size(px(11.5));
                // One element scrolls both axes here, so gpui's per-gesture axis lock is all a trackpad
                // needs (no diagonal drift). `UniformList` is not stateful: the flag goes on its style.
                let mut list = list;
                list.style().restrict_scroll_to_axis = Some(true);
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(list)
                    .child(Scrollbar::vertical(&self.processed.code_scroll))
                    .child(Scrollbar::horizontal(&self.processed.code_scroll))
                    .into_any_element()
            }
        };
        div()
            .flex()
            .flex_col()
            .size_full()
            .min_w(px(0.))
            .child(section_title(title, &c))
            .child(body)
    }

    fn registers(&self, state: &ProcessState, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let frame = self
            .processed
            .rows
            .get(self.processed.selected)
            .map(|r| r.frame);
        let regs: Vec<(String, String)> = frame
            .and_then(|f| state.threads.get(self.processed.thread)?.frames.get(f))
            .map(|f| {
                f.context
                    .valid_registers()
                    .map(|(n, v)| (n.to_owned(), format!("{v:#018x}")))
                    .collect()
            })
            .unwrap_or_default();
        div()
            .flex()
            .flex_col()
            .size_full()
            .border_l_1()
            .border_color(c.border)
            .child(section_title("Registers", &c))
            .child(listing(
                "registers",
                regs,
                &self.processed.regs_scroll,
                ListStyle::Spread,
                cx,
            ))
    }
}

/// The prompt in the source pane: what the file is, where it would come from, and a button — the click is the
/// user's permission; nothing is downloaded before it.
fn fetch_prompt(
    plan: &Plan,
    failed: Option<&String>,
    button: &'static str,
    c: &dtb_ke_ui::theme::PaletteColors,
    cx: &mut Context<DebuggerWindow>,
) -> AnyElement {
    // Children take the pane's width (no `items_start`), so long lines wrap instead of running past it; the button
    // keeps its natural size in a row of its own. A very short pane scrolls.
    div()
        .id("fetch-prompt")
        .size_full()
        .overflow_y_scroll()
        .flex()
        .flex_col()
        .gap(px(8.))
        .p(px(24.))
        .text_size(px(13.))
        .child(div().text_color(c.muted_foreground).child("This source file is not on this machine."))
        .child(div().font_weight(gpui_kit::FontWeight::SEMIBOLD).child(SharedString::from(plan.title.clone())))
        .child(div().text_color(c.muted_foreground).child(SharedString::from(format!(
            "It can be fetched from {}. Only this one file is downloaded — not the repository.",
            plan.source
        ))))
        .when_some(plan.caveat.clone(), |el, caveat| el.child(div().text_size(px(12.)).text_color(c.warn).child(SharedString::from(caveat))))
        .when_some(failed.cloned(), |el, error| {
            el.child(div().text_size(px(12.)).text_color(c.critical).child(SharedString::from(format!("The last attempt failed: {error}"))))
        })
        .child(
            div().flex().flex_none().child(
                Button::new("fetch-source", button)
                    .small()
                    .tone(ButtonTone::Primary)
                    .leading_icon(Icon::Download)
                    .on_click(cx.listener(|this, _, _, cx| this.fetch_source(cx))),
            ),
        )
        .into_any_element()
}

fn thread_name(state: &ProcessState, ix: usize) -> String {
    let Some(t) = state.threads.get(ix) else {
        return format!("Thread {ix}");
    };
    match t.thread_name.as_deref().filter(|n| !n.is_empty()) {
        Some(n) => format!("{ix} · {n} ({:#x})", t.thread_id),
        None => format!("{ix} · {:#x}", t.thread_id),
    }
}

/// One source line with its tokens coloured from the theme's `syntax-*` roles.
fn code_line(
    text: &str,
    tokens: Option<&Vec<(std::ops::Range<usize>, Token)>>,
    c: &dtb_ke_ui::theme::PaletteColors,
) -> AnyElement {
    let color = |t: Token| match t {
        Token::Keyword => c.syntax_keyword,
        Token::String => c.syntax_string,
        Token::Comment => c.syntax_comment,
        Token::Function => c.syntax_function,
        Token::Type => c.syntax_type,
        Token::Number => c.syntax_number,
        Token::Constant => c.syntax_constant,
        Token::Attribute => c.syntax_attribute,
    };
    let styled = StyledText::new(SharedString::from(text.to_owned()));
    match tokens {
        Some(tokens) if !tokens.is_empty() => styled
            .with_highlights(
                tokens
                    .iter()
                    .filter(|(r, _)| {
                        r.end <= text.len()
                            && text.is_char_boundary(r.start)
                            && text.is_char_boundary(r.end)
                    })
                    .map(|(r, t)| {
                        (
                            r.clone(),
                            HighlightStyle {
                                color: Some(color(*t)),
                                ..Default::default()
                            },
                        )
                    }),
            )
            .into_any_element(),
        _ => styled.into_any_element(),
    }
}
