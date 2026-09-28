//! The debugger window: open a dump (dialog, drag-and-drop, CLI arg), resolve its debug files, show the
//! result in four tabs — Verarbeitet (threads, backtrace, registers, source), Rohdaten (streams),
//! Symbole (which debug file each module got, and where from), Build (the build-info stream).
//!
//! One window at a time (`WINDOW`); a dump arriving from the OS (Finder / file association) is loaded into it.

mod info;
mod processed;
mod progress_view;
mod raw;
mod server;
mod symbols;
pub mod widgets;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use dtb_ke_debugger::code::SourceRoots;
use dtb_ke_debugger::process::{Analysis, OpenedDump, analyze};
use dtb_ke_debugger::progress::Progress;
use dtb_ke_debugger::remote::{CacheSource, SymbolServerSource};
use dtb_ke_debugger::{
    DebugFileSource, DirectorySource, DyldSharedCacheSource, ExecutableSource, Resolver,
};
use dtb_ke_ui::components::icon::Icon;
use dtb_ke_ui::components::menu_bar::MenuBar;
use dtb_ke_ui::components::{Button, ButtonTone, Chip, ChipTone, Segmented};
use dtb_ke_ui::theme::{ActiveTheme, Appearance, Theme, ThemeMode};
use gpui_kit::base::{ResizableState, TextSelection, TextSelectionLayer, h_resizable, resizable_panel};
use gpui_kit::{
    App, AppContext, Bounds, ClipboardItem, Context, ExternalPaths, FocusHandle, Focusable,
    InteractiveElement, IntoElement, ParentElement, PathPromptOptions, Pixels, Render,
    SharedString, Size, Styled, Task, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, div, prelude::FluentBuilder, px,
};

use crate::tokio_bridge::Tokio;

gpui_kit::actions!(
    debugger,
    [
        OpenDump,
        AddSymbols,
        ClearSymbolCache,
        CopySelection,
        Quit,
        ShowLogs,
        ToggleSidebar,
        ToggleSource,
        ToggleRegisters,
        About,
        HideApp,
        HideOthers,
        Minimize,
        Zoom,
        ToggleFullscreen,
        CloseWindow,
        OpenRepository,
    ]
);

static WINDOW: dtb_ke_ui::window_registry::WindowRegistry<()> =
    dtb_ke_ui::window_registry::WindowRegistry::new();

const SIDEBAR_WIDTH: Pixels = px(300.);
const SIDEBAR_MIN: Pixels = px(160.);
const SIDEBAR_MAX: Pixels = px(560.);
/// Releasing the divider narrower than this hides the sidebar.
const SIDEBAR_SNAP: Pixels = px(200.);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Processed,
    Raw,
    Symbols,
    Build,
}

const TABS: [(Tab, &str); 4] = [
    (Tab::Processed, "Processed"),
    (Tab::Raw, "Raw dump"),
    (Tab::Symbols, "Symbols"),
    (Tab::Build, "Build"),
];

enum Phase {
    Empty,
    Loading,
    Failed(String),
    Ready,
}

struct Session {
    path: PathBuf,
    opened: Arc<OpenedDump>,
    analysis: Option<Arc<Analysis>>,
    /// Module base → display name (the dump leaves some names empty).
    names: Arc<HashMap<u64, String>>,
}

pub struct DebuggerWindow {
    focus: FocusHandle,
    phase: Phase,
    tab: Tab,
    session: Option<Session>,
    dir_source: Arc<DirectorySource>,
    resolver: Resolver,
    source_roots: SourceRoots,
    /// What the analysis is doing right now (stage, `n / m`, download) — drawn as bars while loading.
    progress: Arc<Progress>,
    tasks: Vec<Task<()>>,
    /// Where fetched sources live: the bare git repositories the partial fetches build, and downloaded `.crate`s.
    sources_cache: PathBuf,
    window_title: String,
    /// System libraries that appear in stack traces but no source could locate, and whether the hint about
    /// them was dismissed.
    unlocated: Vec<String>,
    hint_dismissed: bool,
    sidebar_collapsed: bool,
    sidebar_width: Pixels,
    resizable_state: gpui_kit::Entity<ResizableState>,
    /// The in-app menu strip, drawn only where gpui has no native menu bar (Windows, Linux).
    menu_bar: gpui_kit::Entity<MenuBar>,
    processed: processed::State,
    raw: raw::State,
    symbols: symbols::State,
    info: info::State,
    /// The symbol server: address / token fields, the shared handle the resolver follows, and the hint bar.
    server_ui: server::ServerUi,
    /// The download cache the resolver reads (and "Clear Symbol Cache …" empties).
    symbol_cache: Arc<CacheSource>,
}

/// Open the window (or focus the existing one), optionally with a dump and extra debug-file locations.
pub fn open(cx: &mut App, dump: Option<PathBuf>, symbol_paths: Vec<PathBuf>) {
    if WINDOW.focus((), cx) {
        if let Some(handle) = WINDOW.get(()).and_then(|h| h.downcast::<DebuggerWindow>()) {
            handle
                .update(cx, |this, _window, cx| {
                    for p in symbol_paths {
                        this.dir_source.add_root(p);
                    }
                    if let Some(dump) = dump {
                        this.open_path(dump, cx);
                    }
                })
                .ok();
        }
        return;
    }
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(1280.), px(820.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(dtb_ke_debugger::NAME.into()),
            appears_transparent: dtb_ke_ui::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_minimizable: true,
        window_min_size: Some(Size::new(px(900.), px(560.))),
        ..Default::default()
    };
    match cx.open_window(options, |window, cx| {
        cx.new(|cx| DebuggerWindow::new(window, cx))
    }) {
        Ok(handle) => {
            WINDOW.insert((), handle.into());
            handle
                .update(cx, |this, _window, cx| {
                    for p in symbol_paths {
                        this.dir_source.add_root(p);
                    }
                    // Debug aid: start on a given tab (`DTB_KE_TAB=raw|symbols|build`).
                    this.tab = match std::env::var("DTB_KE_TAB").as_deref() {
                        Ok("raw") => Tab::Raw,
                        Ok("symbols") => Tab::Symbols,
                        Ok("build") => Tab::Build,
                        _ => Tab::Processed,
                    };
                    // Debug aid: start with panes hidden (`DTB_KE_HIDE=sidebar,source,registers`).
                    let hide = std::env::var("DTB_KE_HIDE").unwrap_or_default();
                    this.sidebar_collapsed = hide.contains("sidebar");
                    this.processed.code_hidden = hide.contains("source");
                    this.processed.regs_hidden = hide.contains("registers");
                    // Debug aid: `DTB_KE_SHOW_OS=1` starts with the OS modules listed on the Symbols tab.
                    this.symbols
                        .set_hide_system(std::env::var_os("DTB_KE_SHOW_OS").is_none());
                    if let Some(dump) = dump {
                        this.open_path(dump, cx);
                    }
                })
                .ok();
        }
        Err(err) => log::error!("failed to open the debugger window: {err}"),
    }
}

impl Drop for DebuggerWindow {
    fn drop(&mut self) {
        WINDOW.remove(());
    }
}

impl DebuggerWindow {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // Follow the OS light/dark switch, like the app's own windows.
        Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        cx.observe_window_appearance(window, |_, window, cx| {
            Theme::set_os_appearance(Appearance::from(window.appearance()), cx);
        })
        .detach();

        let dir_source = Arc::new(DirectorySource::new(std::iter::empty()));
        // Cheapest first: the module's own file → files the developer supplied → the download cache →
        // the symbol server (set in the Symbols tab; `DTB_KE_SYMBOL_SERVER` / `DTB_KE_SYMBOL_TOKEN` are the default).
        // A real cache belongs in the OS cache directory, not in the app's data folder nor the (temporary) data root
        // this process runs with (see `main`).
        let cache = Arc::new(CacheSource::new(dtb_ke_debugger::remote::cache_dir()));
        let mut resolver = Resolver::new().with(ExecutableSource);
        resolver.push(dir_source.clone() as Arc<dyn DebugFileSource>);
        // Debug aid: `DTB_KE_NO_DYLD=1` leaves this Mac's cache out, to see the "unlocated system libraries" hint.
        if std::env::var_os("DTB_KE_NO_DYLD").is_none() {
            resolver.push(Arc::new(DyldSharedCacheSource));
        }
        resolver.push(cache.clone() as Arc<dyn DebugFileSource>);
        let symbol_cache = cache.clone();
        // The symbol server is last and always in the chain: it follows `server_ui.handle`, so setting or clearing the
        // address in the Symbols tab takes effect without rebuilding anything (unconfigured = a plain miss).
        let server_ui = server::ServerUi::new(window, cx);
        let progress = Progress::new();
        match SymbolServerSource::with_handle(server_ui.handle.clone(), cache) {
            Ok(server) => resolver.push(Arc::new(server.with_progress(progress.clone()))),
            Err(err) => log::error!("symbol server client: {err:#}"),
        }

        // This checkout is where a dump's workspace paths are re-rooted to by default.
        let checkout = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let source_roots = SourceRoots {
            build_root: None,
            local_roots: std::fs::canonicalize(&checkout).into_iter().collect(),
        };

        let menu_bar = cx.new(|_| MenuBar::new());
        menu_bar.update(cx, |bar, cx| {
            bar.set_menus(crate::app_menu::build(), cx);
            bar.set_hints(crate::app_menu::hints());
        });

        Self {
            menu_bar,
            focus: cx.focus_handle(),
            phase: Phase::Empty,
            tab: Tab::Processed,
            session: None,
            dir_source,
            resolver,
            source_roots,
            progress,
            tasks: Vec::new(),
            sources_cache: dirs::cache_dir()
                .unwrap_or_else(std::env::temp_dir)
                .join("de.philippremy.DTB-KE-Debugger")
                .join("sources"),
            window_title: String::new(),
            unlocated: Vec::new(),
            hint_dismissed: false,
            sidebar_collapsed: false,
            sidebar_width: SIDEBAR_WIDTH,
            resizable_state: cx.new(|_| ResizableState::default()),
            processed: Default::default(),
            raw: Default::default(),
            symbols: Default::default(),
            info: Default::default(),
            server_ui,
            symbol_cache,
        }
    }

    // ── loading ─────────────────────────────────────────────────────────

    fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        log::info!("opening {}", path.display());
        if !dtb_ke_debugger::is_dump(&path) {
            log::error!(
                "{} is not a .{} file",
                path.display(),
                dtb_ke_crash::DUMP_EXTENSION
            );
            self.phase = Phase::Failed(format!(
                "{}: not a crash report (expected a .{} file)",
                path.display(),
                dtb_ke_crash::DUMP_EXTENSION
            ));
            cx.notify();
            return;
        }
        match OpenedDump::open(&path) {
            Ok(opened) => {
                let names = opened
                    .modules
                    .iter()
                    .map(|m| (m.base, m.short_name().to_owned()))
                    .collect::<HashMap<_, _>>();
                self.source_roots.build_root = opened
                    .build
                    .as_ref()
                    .and_then(|b| b.workspace_root())
                    .map(str::to_owned);
                // The OS build names per-build system symbols (Xcode's DeviceSupport, simulator caches).
                let found = dtb_ke_debugger::discover(&opened.system);
                for root in &found.symbol_roots {
                    log::info!(
                        "using Xcode's system symbols for {}: {}",
                        opened.system,
                        root.display()
                    );
                    self.dir_source.add_root(root.clone());
                }
                if !found.cache_dirs.is_empty() {
                    let opened_caches = dtb_ke_debugger::add_caches_from(&found.cache_dirs);
                    log::info!(
                        "simulator cache dir(s) for {}: {} new cache(s)",
                        opened.system,
                        opened_caches
                    );
                }
                self.session = Some(Session {
                    path,
                    opened: Arc::new(opened),
                    analysis: None,
                    names: Arc::new(names),
                });
                self.hint_dismissed = false;
                self.server_ui.hint_dismissed = false;
                self.server_ui.hint = None;
                self.unlocated.clear();
                self.processed = Default::default();
                self.raw = Default::default();
                self.symbols = Default::default();
                self.info = Default::default();
                self.analyze(cx);
            }
            Err(err) => {
                log::error!("cannot open {}: {err:#}", path.display());
                self.phase = Phase::Failed(format!("{}: {err}", path.display()));
                cx.notify();
            }
        }
    }

    fn analyze(&mut self, cx: &mut Context<Self>) {
        let Some(session) = &self.session else { return };
        let opened = session.opened.clone();
        let resolver = self.resolver.clone();
        let progress = self.progress.clone();
        progress.clear();
        self.phase = Phase::Loading;
        self.tasks.clear();

        let work = Tokio::spawn_result(
            cx,
            async move { analyze(&opened, &resolver, &progress).await },
        );
        self.tasks.push(cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| this.finish(result, cx)).ok();
        }));
        // Repaint while working so the progress line moves.
        self.tasks.push(cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                let done = this
                    .update(cx, |this, cx| {
                        cx.notify();
                        !matches!(this.phase, Phase::Loading)
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        }));
        cx.notify();
    }

    fn finish(&mut self, result: anyhow::Result<Analysis>, cx: &mut Context<Self>) {
        self.progress.clear();
        match result {
            Ok(analysis) => {
                let analysis = Arc::new(analysis);
                if let Some(s) = &mut self.session {
                    s.analysis = Some(analysis.clone());
                }
                log::info!(
                    "analysis done: {} thread(s), {} module(s) with debug files",
                    analysis.state.threads.len(),
                    analysis.resolution.found().count()
                );
                self.unlocated = self.unlocated_system_libraries(&analysis);
                self.server_ui.hint = self.server_hint_for(&analysis);
                if !self.unlocated.is_empty() {
                    log::info!(
                        "system libraries in stack frames that no source located: {}",
                        self.unlocated.join(", ")
                    );
                }
                self.phase = Phase::Ready;
                self.processed_analysis_ready(cx);
            }
            Err(err) => {
                log::error!("analysis failed: {err:#}");
                self.phase = Phase::Failed(format!("Analysis failed: {err:#}"));
            }
        }
        cx.notify();
    }

    /// Names of OS libraries that some stack frame sits in but that no source could locate.
    fn unlocated_system_libraries(&self, analysis: &Analysis) -> Vec<String> {
        use dtb_ke_crash::syshints::Quality;
        use dtb_ke_debugger::Outcome;
        use minidump::Module;
        let Some(session) = &self.session else {
            return Vec::new();
        };
        // The crashed machine named these frames itself, from full symbols, for every frame: nothing left to ask for.
        if session
            .opened
            .hints
            .as_ref()
            .is_some_and(|h| h.quality == Quality::Exact && h.complete)
        {
            return Vec::new();
        }
        let mut names = std::collections::BTreeSet::new();
        for frame in analysis.state.threads.iter().flat_map(|t| &t.frames) {
            let Some(module) = &frame.module else {
                continue;
            };
            let base = module.base_address();
            let missing = analysis.resolution.for_module(base).is_some_and(|m| {
                m.module.is_system() && matches!(m.outcome, Outcome::Missing { .. })
            });
            if missing {
                names.insert(
                    session
                        .names
                        .get(&base)
                        .cloned()
                        .unwrap_or_else(|| module.code_file().into_owned()),
                );
            }
        }
        names.into_iter().collect()
    }

    fn analysis(&self) -> Option<&Arc<Analysis>> {
        self.session.as_ref()?.analysis.as_ref()
    }

    fn prompt_open(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("Open crash report".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                if let Some(path) = paths.into_iter().next() {
                    this.update(cx, |this, cx| this.open_path(path, cx)).ok();
                }
            }
        })
        .detach();
    }

    fn prompt_add_symbols(&mut self, cx: &mut Context<Self>) {
        let rx = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: true,
            multiple: true,
            prompt: Some("Add debug files".into()),
        });
        cx.spawn(async move |this, cx| {
            if let Ok(Ok(Some(paths))) = rx.await {
                this.update(cx, |this, cx| this.add_symbol_paths(paths, cx))
                    .ok();
            }
        })
        .detach();
    }

    fn add_symbol_paths(&mut self, paths: Vec<PathBuf>, cx: &mut Context<Self>) {
        // Anything supplied may be a folder of images / dSYMs / PDBs, or a dyld shared cache (a simulator
        // runtime's, one from an IPSW) — try both readings.
        let caches = dtb_ke_debugger::add_caches_from(&paths);
        if caches > 0 {
            log::info!("{caches} dyld cache(s) added");
        }
        for p in paths {
            log::info!("adding symbol path {}", p.display());
            self.dir_source.add_root(p);
        }
        self.analyze(cx);
    }

    fn dropped(&mut self, paths: &ExternalPaths, cx: &mut Context<Self>) {
        let mut paths = paths.paths().to_vec();
        // A dropped `.dtbkedmp` opens; anything else (dSYM bundle, PDB, directory) is a symbol source.
        if let Some(i) = paths.iter().position(|p| dtb_ke_debugger::is_dump(p)) {
            let dump = paths.remove(i);
            for p in paths {
                self.dir_source.add_root(p);
            }
            self.open_path(dump, cx);
        } else {
            self.add_symbol_paths(paths, cx);
        }
    }

    // ── sidebar ─────────────────────────────────────────────────────────

    fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_collapsed = !self.sidebar_collapsed;
        if !self.sidebar_collapsed {
            // Re-open at the remembered width instead of whatever the split last measured.
            self.resizable_state.update(cx, |state, _| state.clear());
        }
        cx.notify();
    }

    /// `[sidebar | content]` as a resizable split (clamped to `SIDEBAR_MIN..SIDEBAR_MAX`; released below
    /// `SIDEBAR_SNAP` it hides), or just the content when the sidebar is hidden. Same mechanism as the app's shell.
    fn with_sidebar(
        &mut self,
        sidebar: gpui_kit::AnyElement,
        content: gpui_kit::AnyElement,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let c = cx.theme().color;
        let row = div().flex().flex_1().min_h(px(0.));
        if self.sidebar_collapsed {
            return row
                .child(div().flex().flex_1().min_w(px(0.)).child(content))
                .into_any_element();
        }
        let weak = cx.weak_entity();
        let split = h_resizable("main-split")
            .with_state(&self.resizable_state)
            .on_resize(move |state, _window, cx| {
                let width = state
                    .read(cx)
                    .sizes()
                    .first()
                    .copied()
                    .unwrap_or(SIDEBAR_WIDTH);
                weak.update(cx, |this, cx| {
                    if width < SIDEBAR_SNAP {
                        this.sidebar_collapsed = true;
                    } else {
                        this.sidebar_width = width;
                    }
                    cx.notify();
                })
                .ok();
            })
            .child(
                resizable_panel()
                    .size(self.sidebar_width)
                    .size_range(SIDEBAR_MIN..SIDEBAR_MAX)
                    .flex_none()
                    .child(div().size_full().bg(c.chrome).child(sidebar)),
            )
            .child(resizable_panel().child(div().flex().size_full().min_w(px(0.)).child(content)));
        row.child(split).into_any_element()
    }

    // ── render ──────────────────────────────────────────────────────────

    fn toolbar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let status = self.analysis().map(|a| {
            let found = a.resolution.found().count();
            let total = a.resolution.modules.len();
            (
                format!("{found}/{total}"),
                if found == 0 {
                    ChipTone::Warn
                } else {
                    ChipTone::Ok
                },
            )
        });
        let tab = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
        let me = cx.entity();

        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(8.))
            .px(px(14.))
            .h(px(44.))
            .border_b_1()
            .border_color(c.border)
            .when(self.has_sidebar(), |el| {
                el.child(
                    Button::icon("toggle-sidebar", Icon::PanelLeft)
                        .small()
                        .tone(ButtonTone::Ghost)
                        .tooltip("Show / hide the sidebar")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_sidebar(cx))),
                )
            })
            .child(
                Button::new("open", "Open …")
                    .small()
                    .tone(ButtonTone::Primary)
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_open(cx))),
            )
            .child(
                Button::new("add-symbols", "Debug files …")
                    .small()
                    .tone(ButtonTone::Secondary)
                    .leading_icon(Icon::Plus)
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_add_symbols(cx))),
            )
            // How many modules found a debug file — just `found/total`, as tall as the button beside it.
            .when_some(status, |el, (label, tone)| {
                el.child(Chip::new(label).tone(tone).mono().height(px(24.)))
            })
            // Something the dump could not resolve: a warning icon that shows / hides the explanation, so a
            // dismissed hint can always be read again.
            .when(self.hint_available(), |el| {
                let shown = !self.hint_dismissed;
                el.child(
                    Button::icon("toggle-hint", Icon::Warning)
                        .small()
                        .tone(if shown {
                            ButtonTone::Secondary
                        } else {
                            ButtonTone::Ghost
                        })
                        .foreground(c.warn)
                        .tooltip(if shown {
                            "Hide the system-symbol hint"
                        } else {
                            "Show the system-symbol hint"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.hint_dismissed = !this.hint_dismissed;
                            cx.notify();
                        })),
                )
            })
            // Modules with only a symbol table: the same idea for the symbol server.
            .when(self.server_hint_available(), |el| {
                let shown = !self.server_ui.hint_dismissed;
                el.child(
                    Button::icon("toggle-server-hint", Icon::Download)
                        .small()
                        .tone(if shown {
                            ButtonTone::Secondary
                        } else {
                            ButtonTone::Ghost
                        })
                        .foreground(c.warn)
                        .tooltip(if shown {
                            "Hide the symbol-server hint"
                        } else {
                            "Show the symbol-server hint"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.server_ui.hint_dismissed = !this.server_ui.hint_dismissed;
                            cx.notify();
                        })),
                )
            })
            .child(div().flex_1())
            .child(
                Segmented::new("tabs", TABS.iter().map(|(_, l)| *l), tab).on_select(
                    move |ix, _window, cx| {
                        me.update(cx, |this, cx| {
                            this.tab = TABS[ix].0;
                            cx.notify();
                        });
                    },
                ),
            )
            // Far right: the appearance, cycling System → Light → Dark (the icon shows the current mode). It persists into
            // this process's temporary settings only — never into the app's `Settings.toml`.
            .child({
                let mode = Theme::mode(cx);
                let (icon, name) = match mode {
                    ThemeMode::System => (Icon::Monitor, "System"),
                    ThemeMode::Light => (Icon::Sun, "Light"),
                    ThemeMode::Dark => (Icon::Moon, "Dark"),
                };
                Button::icon("theme", icon)
                    .small()
                    .tone(ButtonTone::Ghost)
                    .tooltip(format!("Appearance: {name} — click to change"))
                    .on_click(|_, _, cx| Theme::set_mode(Theme::mode(cx).cycled(), cx))
            })
    }

    /// A slim strip along the bottom edge with the source / register pane toggles as small icon buttons —
    /// out of the way, like the layout controls in Instruments. Shown on the Processed tab only; a shown
    /// pane's button looks pressed. (The sidebar toggle stays in the toolbar.)
    fn status_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let processed = self.tab == Tab::Processed;
        let tone = |on: bool| {
            if on {
                ButtonTone::Secondary
            } else {
                ButtonTone::Ghost
            }
        };
        let (code_on, regs_on) = (!self.processed.code_hidden, !self.processed.regs_hidden);

        div()
            .flex()
            .flex_none()
            .items_center()
            .justify_end()
            .gap(px(2.))
            .px(px(10.))
            .h(px(26.))
            .border_t_1()
            .border_color(c.border)
            .bg(c.chrome)
            .when(processed, |el| {
                el.child(
                    Button::icon("toggle-source", Icon::Code)
                        .x_small()
                        .tone(tone(code_on))
                        .tooltip("Show / hide the source pane")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_code(cx))),
                )
                .child(
                    Button::icon("toggle-registers", Icon::Cpu)
                        .x_small()
                        .tone(tone(regs_on))
                        .tooltip("Show / hide the register pane")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_registers(cx))),
                )
            })
    }

    /// Shown when system libraries in the stack traces could not be located: says what the dump ran on and
    /// what to supply. The rest of the app keeps working without them.
    fn system_hint(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let facts = self
            .session
            .as_ref()
            .map(|s| s.opened.system.to_string())
            .unwrap_or_default();
        let count = self.unlocated.len();
        let mut sample = self
            .unlocated
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if count > 3 {
            sample.push_str(", …");
        }
        // What the crashed machine already told us about these frames, if anything.
        let from_dump = self.session.as_ref().and_then(|s| s.opened.hints.as_ref()).map(|h| {
            use dtb_ke_crash::syshints::Quality;
            let quality = match h.quality {
                Quality::Exact => "exact",
                Quality::Approximate => "approximate — exported symbols only",
            };
            let state = if h.complete { "complete".to_owned() } else { format!("incomplete: {}", h.incomplete_reason) };
            format!(
                " The dump carries names the crashing machine resolved for them ({quality}; {state}; {} of {} frames) — \
                 shown with \"≈\" where approximate.",
                h.frames_resolved, h.frames_total
            )
        });
        let message = format!(
            "{count} system librar{} in the stack traces could not be located ({sample}). This dump ran {facts}.{} \
             To get exact names, supply that build's system symbols: a dyld shared cache (a simulator runtime's, or one \
             from an IPSW), or Xcode's device-support \"Symbols\" folder for it.",
            if count == 1 { "y" } else { "ies" },
            from_dump.unwrap_or_default(),
        );
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(8.))
            .border_b_1()
            .border_color(c.border)
            .bg(widgets::tint(c.warn, 0.12))
            .child(
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .text_size(px(12.))
                    .child(SharedString::from(message)),
            )
            .child(
                Button::new("hint-choose", "Choose files …")
                    .small()
                    .tone(ButtonTone::Secondary)
                    .on_click(cx.listener(|this, _, _, cx| this.prompt_add_symbols(cx))),
            )
            .child(
                Button::new("hint-dismiss", "Dismiss")
                    .small()
                    .tone(ButtonTone::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.hint_dismissed = true;
                        cx.notify();
                    })),
            )
    }

    /// There is a system-symbol hint to show (system libraries in the stacks that no source could locate).
    fn hint_available(&self) -> bool {
        matches!(self.phase, Phase::Ready) && !self.unlocated.is_empty()
    }

    /// Whether the current view has a sidebar to show or hide.
    fn has_sidebar(&self) -> bool {
        matches!(self.phase, Phase::Ready) && matches!(self.tab, Tab::Processed | Tab::Raw)
    }

    fn body(&mut self, window: &mut Window, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let c = cx.theme().color;
        match &self.phase {
            Phase::Empty => widgets::placeholder(
                "Drop a minidump here, or choose \"Open …\". Debug files (dSYM, PDB, binaries, whole folders) can be dropped in \
                 as well.",
                &c,
            ),
            Phase::Failed(msg) => widgets::placeholder(msg.clone(), &c),
            Phase::Loading => div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .p(px(24.))
                .child(progress_view::loading_card(&self.progress.snapshot(), cx))
                .into_any_element(),
            Phase::Ready => match self.tab {
                Tab::Processed => self.render_processed(window, cx),
                Tab::Raw => self.render_raw(cx),
                Tab::Symbols => self.render_symbols(cx),
                Tab::Build => self.render_info(cx),
            },
        }
    }
}

impl Render for DebuggerWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let font = cx.theme().skin.font_family();
        let lead = dtb_ke_ui::skin::titlebar::content_leading_inset(window);

        // "<name> (<file>)" — in the OS title bar and in our own title strip.
        let title = match self.session.as_ref().and_then(|s| s.path.file_name()) {
            Some(name) => format!("{} ({})", dtb_ke_debugger::NAME, name.to_string_lossy()),
            None => dtb_ke_debugger::NAME.to_owned(),
        };
        if self.window_title != title {
            window.set_window_title(&title);
            self.window_title = title.clone();
        }

        if window.is_window_active() && window.focused(cx).is_none() {
            let handle = self.focus.clone();
            window.focus(&handle, cx);
        }

        div()
            .track_focus(&self.focus)
            .key_context("Debugger")
            .on_action(cx.listener(|this, _: &OpenDump, _, cx| this.prompt_open(cx)))
            .on_action(cx.listener(|this, _: &AddSymbols, _, cx| this.prompt_add_symbols(cx)))
            .on_action(cx.listener(|this, _: &ClearSymbolCache, window, cx| {
                this.clear_symbol_cache(window, cx)
            }))
            .on_action(cx.listener(|_, _: &CopySelection, window, cx| {
                let text = TextSelection::selected_text(window, cx);
                if !text.is_empty() {
                    cx.write_to_clipboard(ClipboardItem::new_string(text));
                }
            }))
            .on_action(cx.listener(|this, _: &ToggleSidebar, _, cx| this.toggle_sidebar(cx)))
            .on_action(cx.listener(|this, _: &ToggleSource, _, cx| this.toggle_code(cx)))
            .on_action(cx.listener(|this, _: &ToggleRegisters, _, cx| this.toggle_registers(cx)))
            .on_action(cx.listener(|_, _: &CloseWindow, window, _| window.remove_window()))
            .on_action(cx.listener(|_, _: &Minimize, window, _| window.minimize_window()))
            .on_action(cx.listener(|_, _: &Zoom, window, _| window.zoom_window()))
            .on_action(cx.listener(|_, _: &ToggleFullscreen, window, _| window.toggle_fullscreen()))
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| this.dropped(paths, cx)))
            .size_full()
            .flex()
            .flex_col()
            .bg(c.background)
            .text_color(c.foreground)
            .when_some(font, |el, family| el.font_family(family))
            .child(TextSelectionLayer)
            .when(dtb_ke_ui::skin::menu::in_app(cx), |el| {
                el.child(
                    div()
                        .flex()
                        .flex_none()
                        .items_center()
                        .bg(c.chrome)
                        .border_b_1()
                        .border_color(c.border)
                        .child(self.menu_bar.clone()),
                )
            })
            .child(
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
                    .child(SharedString::from(title)),
            )
            .child(self.toolbar(cx))
            .when(self.hint_available() && !self.hint_dismissed, |el| {
                el.child(self.system_hint(cx))
            })
            .when(
                self.server_hint_available() && !self.server_ui.hint_dismissed,
                |el| el.child(self.server_hint_bar(cx)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_h(px(0.))
                    .child(self.body(window, cx)),
            )
            .when(
                matches!(self.phase, Phase::Ready) && self.tab == Tab::Processed,
                |el| el.child(self.status_bar(cx)),
            )
    }
}

impl Focusable for DebuggerWindow {
    fn focus_handle(&self, _cx: &App) -> FocusHandle {
        self.focus.clone()
    }
}
