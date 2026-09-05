//! The About window and its three companions — Acknowledgements (every
//! dependency + its embedded licence), Build-Informationen (VCS / toolchain /
//! profile metadata from `build.rs`), and Lizenz (the app's AGPL-3.0 text).
//!
//! Each is a separate window, Xcode-style. At most one of each kind is open at a
//! time; re-triggering just activates the existing one.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::{Arc, OnceLock};

use gpui::{
    AnyElement, AnyWindowHandle, App, AppContext, Bounds, Context, Entity, FontWeight, Image,
    ImageFormat, InteractiveElement, IntoElement, ParentElement, Pixels, Render, ScrollHandle,
    Size, StatefulInteractiveElement, Styled, TitlebarOptions, Window, WindowBounds, WindowKind,
    WindowOptions, div, img, prelude::FluentBuilder, px, size,
};
use gpui_base::{Scrollbar, VirtualListScrollHandle, v_virtual_list};

use crate::build_info::{self, D};
use crate::components::{Button, ButtonTone};
use crate::theme::ActiveTheme;

const APP_NAME: &str = "DTB Kampfrichtereinsatzpläne";
const COPYRIGHT: &str = "© 2026 Philipp Remy. Freie Software, lizenziert unter der GNU Affero General Public License v3.";

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    About,
    Acknowledgements,
    BuildInfo,
    License,
}

impl Kind {
    fn title(self) -> &'static str {
        match self {
            Kind::About => "Über DTB Kampfrichtereinsatzpläne",
            Kind::Acknowledgements => "Danksagungen",
            Kind::BuildInfo => "Build-Informationen",
            Kind::License => "Lizenzvereinbarung",
        }
    }

    fn size(self) -> Size<gpui::Pixels> {
        match self {
            Kind::About => Size::new(px(600.), px(260.)),
            Kind::Acknowledgements => Size::new(px(620.), px(680.)),
            Kind::BuildInfo => Size::new(px(560.), px(640.)),
            Kind::License => Size::new(px(600.), px(720.)),
        }
    }
}

thread_local! {
    static OPEN: RefCell<HashMap<Kind, AnyWindowHandle>> = RefCell::new(HashMap::new());
}

/// Open (or focus) the About window. Entry point for the `app::About` action.
pub fn open(cx: &mut App) {
    open_kind(cx, Kind::About);
}

/// Debug aid (`DTB_KE_ABOUT=ack|build|license`): open one companion window
/// directly. Any other value opens the About window itself.
pub fn open_named(cx: &mut App, name: &str) {
    let kind = match name {
        "ack" | "acknowledgements" | "danksagungen" => Kind::Acknowledgements,
        "build" | "buildinfo" => Kind::BuildInfo,
        "license" | "lizenz" => Kind::License,
        _ => Kind::About,
    };
    open_kind(cx, kind);
}

fn open_kind(cx: &mut App, kind: Kind) {
    // Already open? Just bring it forward.
    let existing = OPEN.with(|m| m.borrow().get(&kind).copied());
    if let Some(handle) = existing {
        if handle
            .update(cx, |_, window, _| window.activate_window())
            .is_ok()
        {
            return;
        }
        OPEN.with(|m| {
            m.borrow_mut().remove(&kind);
        });
    }

    let opts = window_options(kind, cx);
    let handle = match kind {
        Kind::About => cx
            .open_window(opts, |_, cx| cx.new(|_| AboutWindow { kind }))
            .map(Into::into),
        other => cx
            .open_window(opts, |_, cx| cx.new(|_| InfoWindow::new(other)))
            .map(Into::into),
    };
    match handle {
        Ok(handle) => {
            OPEN.with(|m| {
                m.borrow_mut().insert(kind, handle);
            });
        }
        Err(err) => log::error!(
            "{}-Fenster konnte nicht geöffnet werden: {err}",
            kind.title()
        ),
    }
}

fn window_options(kind: Kind, cx: &mut App) -> WindowOptions {
    WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            kind.size(),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some(kind.title().into()),
            appears_transparent: crate::skin::window::secondary_window_appears_transparent(),
            ..Default::default()
        }),
        kind: WindowKind::Normal,
        is_resizable: !matches!(kind, Kind::About),
        is_minimizable: false,
        ..Default::default()
    }
}

fn deregister(kind: Kind) {
    OPEN.with(|m| {
        m.borrow_mut().remove(&kind);
    });
}

// ── the About window ─────────────────────────────────────────────────────

struct AboutWindow {
    kind: Kind,
}

impl Drop for AboutWindow {
    fn drop(&mut self) {
        deregister(self.kind);
    }
}

impl Render for AboutWindow {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let version = format!(
            "Version {}{}",
            build_info::APP_VERSION,
            match (build_info::COMMIT, build_info::PROFILE) {
                ("", _) => String::new(),
                (commit, profile) => format!(" ({commit} · {profile})"),
            }
        );

        div()
            .size_full()
            .overflow_hidden()
            .bg(c.background)
            .text_color(c.foreground)
            .pt(px(48.))
            .px(px(28.))
            .pb(px(20.))
            .flex()
            .gap(px(24.))
            .child(app_icon(128.))
            .child(
                // `min_w(0)` lets this flex child shrink below its content's
                // width so the text wraps instead of overflowing the window.
                div()
                    .flex_1()
                    .min_w(px(0.))
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .w_full()
                            .text_size(px(26.))
                            .font_weight(gpui::FontWeight::BOLD)
                            .child(APP_NAME),
                    )
                    .child(
                        div()
                            .w_full()
                            .text_size(px(13.))
                            .text_color(c.muted_foreground)
                            .child(version),
                    )
                    .child(
                        div()
                            .w_full()
                            .mt(px(18.))
                            .text_size(px(11.))
                            .text_color(c.muted_foreground)
                            .child(COPYRIGHT),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .justify_end()
                            .gap(px(8.))
                            .child(
                                Button::new("ack", "Danksagungen")
                                    .tone(ButtonTone::Secondary)
                                    .on_click(|_, _, cx| open_kind(cx, Kind::Acknowledgements)),
                            )
                            .child(
                                Button::new("meta", "Build-Infos")
                                    .tone(ButtonTone::Secondary)
                                    .on_click(|_, _, cx| open_kind(cx, Kind::BuildInfo)),
                            )
                            .child(
                                Button::new("lic", "Lizenz")
                                    .tone(ButtonTone::Secondary)
                                    .on_click(|_, _, cx| open_kind(cx, Kind::License)),
                            ),
                    ),
            )
    }
}

/// The app icon master, shared with `dtb-ke-bundle` (`assets/icons/`).
const APP_ICON_PNG: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../assets/icons/AppIcon.png"
));

/// The decoded icon, kept alive so gpui's image cache stays warm across renders.
fn app_icon_image() -> Arc<Image> {
    static IMG: OnceLock<Arc<Image>> = OnceLock::new();
    IMG.get_or_init(|| Arc::new(Image::from_bytes(ImageFormat::Png, APP_ICON_PNG.to_vec())))
        .clone()
}

/// The app icon, masked to the macOS rounded-square silhouette (a no-op if the
/// art already carries transparent corners).
fn app_icon(size: f32) -> impl IntoElement {
    div()
        .flex_none()
        .size(px(size))
        .rounded(px(size * 0.2237))
        .overflow_hidden()
        .child(img(app_icon_image()).size_full())
}

// ── the three companion windows ──────────────────────────────────────────

/// Acknowledgements row heights (px, border-box) — the virtual list needs each
/// row's exact height up front, so both parts are drawn at a forced size.
const ACK_ROW_H: f32 = 39.0;
/// The expanded licence panel is capped at this and scrolls inside itself.
const ACK_LICENSE_BOX_H: f32 = 220.0;

struct InfoWindow {
    kind: Kind,
    /// Acknowledgements only: which rows are expanded to show their licence.
    expanded: Rc<RefCell<HashSet<usize>>>,
    /// Build-Info / Lizenz scroll here (plain columns).
    scroll: ScrollHandle,
    /// Acknowledgements only: the virtual dependency list's scroll position.
    ack_scroll: VirtualListScrollHandle,
    /// Acknowledgements only: per-row heights, rebuilt whenever a row is
    /// expanded or collapsed (`ACK_ROW_H`, plus `ACK_LICENSE_BOX_H` when open).
    ack_sizes: Rc<Vec<Size<Pixels>>>,
    /// Acknowledgements only: the inner licence panels' own scroll positions,
    /// kept across frames so a row scrolled out of view and back is unchanged.
    ack_license_scrolls: HashMap<usize, ScrollHandle>,
    /// Lizenz only: the AGPL text reflowed into natural paragraphs.
    license_blocks: Vec<LicenseBlock>,
}

/// Height of every acknowledgements row for the current expansion state.
fn ack_row_sizes(expanded: &HashSet<usize>) -> Vec<Size<Pixels>> {
    (0..build_info::DEPENDENCIES.len())
        .map(|ix| {
            let h = if expanded.contains(&ix) {
                ACK_ROW_H + ACK_LICENSE_BOX_H
            } else {
                ACK_ROW_H
            };
            // Width is ignored for a vertical list (it takes the list's width).
            size(px(1.), px(h))
        })
        .collect()
}

impl Drop for InfoWindow {
    fn drop(&mut self) {
        deregister(self.kind);
    }
}

impl InfoWindow {
    fn new(kind: Kind) -> Self {
        let license_blocks = if kind == Kind::License {
            reflow_license(build_info::APP_LICENSE_TEXT)
        } else {
            Vec::new()
        };
        Self {
            kind,
            expanded: Rc::new(RefCell::new(HashSet::new())),
            scroll: ScrollHandle::new(),
            ack_scroll: VirtualListScrollHandle::new(),
            ack_sizes: Rc::new(ack_row_sizes(&HashSet::new())),
            ack_license_scrolls: HashMap::new(),
            license_blocks,
        }
    }
}

impl Render for InfoWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let lead = crate::skin::titlebar::content_leading_inset(window);

        let body = match self.kind {
            Kind::Acknowledgements => self.acknowledgements(cx),
            Kind::BuildInfo => self.build_info(&c).into_any_element(),
            Kind::License => self.license(&c).into_any_element(),
            Kind::About => div().into_any_element(),
        };

        div()
            .size_full()
            .bg(c.background)
            .text_color(c.foreground)
            .flex()
            .flex_col()
            .child(
                // Title strip — mirrors the preview window's; leaves room for
                // the traffic lights on macOS.
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
                    .child(self.kind.title()),
            )
            .child(div().flex_1().min_h(px(0.)).child(body))
    }
}

impl InfoWindow {
    fn acknowledgements(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let expanded = self.expanded.clone();
        let entity = cx.entity();
        let c = cx.theme().color;
        let sizes = self.ack_sizes.clone();

        // Virtualised: only the visible rows are laid out. Row heights are known
        // up front (`ack_row_sizes`) because both the header strip and the
        // expanded licence panel are drawn at a forced height; the licence text
        // scrolls inside its own capped panel rather than growing the row.
        div()
            .relative()
            .size_full()
            .overflow_hidden()
            .child(
                v_virtual_list(
                    cx.entity(),
                    "ack-list",
                    sizes,
                    move |this, range, _window, _cx| {
                        range
                            .map(|ix| {
                                let lic_scroll = this
                                    .ack_license_scrolls
                                    .entry(ix)
                                    .or_insert_with(ScrollHandle::new)
                                    .clone();
                                let is_open = expanded.borrow().contains(&ix);
                                dep_row(ix, c, is_open, entity.clone(), lic_scroll)
                            })
                            .collect()
                    },
                )
                .track_scroll(&self.ack_scroll)
                .size_full(),
            )
            .child(Scrollbar::vertical(&self.ack_scroll))
            .into_any_element()
    }

    fn build_info(&self, c: &crate::theme::PaletteColors) -> impl IntoElement {
        div()
            .id("meta-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(20.))
            .py(px(14.))
            .flex()
            .flex_col()
            .children(build_info::rows().into_iter().map(|(label, value)| {
                div()
                    .flex()
                    .py(px(6.))
                    .border_b_1()
                    .border_color(c.border)
                    .text_size(px(12.5))
                    .child(
                        div()
                            .w(px(180.))
                            .flex_none()
                            .text_color(c.muted_foreground)
                            .child(label),
                    )
                    .child(div().flex_1().child(value))
            }))
    }

    fn license(&self, c: &crate::theme::PaletteColors) -> impl IntoElement {
        div()
            .id("license-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .px(px(28.))
            .py(px(16.))
            .flex()
            .flex_col()
            .items_center()
            .child(
                div()
                    .w_full()
                    .max_w(px(540.))
                    .text_size(px(12.))
                    .line_height(px(18.))
                    .text_color(c.foreground)
                    .children(self.license_blocks.iter().map(|b| license_block_el(b, c))),
            )
    }
}

/// One reflowed span of the licence text.
#[derive(Clone)]
enum LicenseBlock {
    /// A deeply-indented display line — centred (document title, "Preamble", …).
    Heading(String),
    /// A numbered clause header ("0. Definitions.") — left, semibold.
    Section(String),
    /// A body paragraph. `indent` marks lettered sub-items (a) / b) / …).
    Para { text: String, indent: bool },
}

/// Turn the hard-wrapped canonical AGPL text into paragraphs that reflow to the
/// window width. Continuation lines within a block are joined; blank lines keep
/// their paragraph break; deep indentation marks a centred heading.
fn reflow_license(src: &str) -> Vec<LicenseBlock> {
    let lead = |l: &str| l.len() - l.trim_start().len();
    let mut blocks = Vec::new();
    let mut para: Vec<&str> = Vec::new();

    let flush = |blocks: &mut Vec<LicenseBlock>, para: &mut Vec<&str>| {
        if para.is_empty() {
            return;
        }
        let lines = std::mem::take(para);

        // A block whose every line is deeply indented is a centred display
        // heading — emit one per line so multi-line titles stack.
        if lines.iter().all(|l| lead(l) >= 10) {
            blocks.extend(
                lines
                    .iter()
                    .map(|l| LicenseBlock::Heading(l.trim().to_string())),
            );
            return;
        }

        let joined = lines
            .iter()
            .flat_map(|l| l.split_whitespace())
            .collect::<Vec<_>>()
            .join(" ");

        // A lone short "N. Title." line is a clause header.
        if lines.len() == 1 {
            let t = lines[0].trim();
            let is_section = t.len() < 55
                && t.split_once(". ")
                    .is_some_and(|(n, _)| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            if is_section {
                blocks.push(LicenseBlock::Section(t.to_string()));
                return;
            }
        }

        let indent = lines.iter().map(|l| lead(l)).min().unwrap_or(0) >= 4;
        blocks.push(LicenseBlock::Para {
            text: joined,
            indent,
        });
    };

    for line in src.lines() {
        if line.trim().is_empty() {
            flush(&mut blocks, &mut para);
        } else {
            para.push(line);
        }
    }
    flush(&mut blocks, &mut para);
    blocks
}

fn license_block_el(b: &LicenseBlock, c: &crate::theme::PaletteColors) -> gpui::Div {
    match b {
        LicenseBlock::Heading(t) => div()
            .w_full()
            .mt(px(18.))
            .mb(px(6.))
            .text_center()
            .font_weight(FontWeight::BOLD)
            .child(t.clone()),
        LicenseBlock::Section(t) => div()
            .w_full()
            .mt(px(20.))
            .mb(px(4.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(c.foreground)
            .child(t.clone()),
        LicenseBlock::Para { text, indent } => div()
            .w_full()
            .mb(px(9.))
            .when(*indent, |el| el.pl(px(22.)))
            .child(text.clone()),
    }
}

fn dep_row(
    ix: usize,
    c: crate::theme::PaletteColors,
    is_open: bool,
    entity: Entity<InfoWindow>,
    license_scroll: ScrollHandle,
) -> impl IntoElement {
    let dep: &D = &build_info::DEPENDENCIES[ix];
    let license = build_info::dep_license(dep);

    let toggle = move |_: &gpui::ClickEvent, _: &mut Window, cx: &mut App| {
        entity.update(cx, |this, cx| {
            {
                let mut e = this.expanded.borrow_mut();
                if !e.remove(&ix) {
                    e.insert(ix);
                }
            }
            // The virtual list keys layout off these — rebuild on every toggle.
            this.ack_sizes = Rc::new(ack_row_sizes(&this.expanded.borrow()));
            cx.notify();
        });
    };

    div()
        .w_full()
        .flex()
        .flex_col()
        .child(
            div()
                .id(ix)
                .h(px(ACK_ROW_H))
                .flex_none()
                .border_b_1()
                .border_color(c.border)
                .flex()
                .items_center()
                .gap(px(8.))
                .px(px(20.))
                .cursor_pointer()
                .hover(|el| el.bg(c.selection))
                .on_click(toggle)
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.5))
                        .child(format!("{} {}", dep.name, dep.version)),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(11.))
                        .text_color(c.muted_foreground)
                        .child(dep.spdx.unwrap_or("Lizenz unbekannt").to_string()),
                ),
        )
        .when(is_open, |el| {
            el.child(
                div()
                    .h(px(ACK_LICENSE_BOX_H))
                    .flex_none()
                    .relative()
                    .bg(c.surface)
                    .border_b_1()
                    .border_color(c.border)
                    .child(
                        div()
                            .id(("license", ix))
                            .size_full()
                            .overflow_y_scroll()
                            .track_scroll(&license_scroll)
                            // Keep the wheel here — without this the outer
                            // virtual list scrolls too (gpui's scroll handler
                            // doesn't stop propagation; `occlude` drops the
                            // list's hitbox from the scroll hit-test).
                            .occlude()
                            .px(px(20.))
                            .py(px(14.))
                            .text_size(px(10.5))
                            .text_color(c.muted_foreground)
                            .child(
                                license
                                    .map(str::to_string)
                                    .unwrap_or_else(|| "Kein Lizenztext eingebettet.".to_string()),
                            ),
                    )
                    .child(Scrollbar::vertical(&license_scroll)),
            )
        })
}
