//! "Symbols": which debug file each module got, from where, or why none was found.
//!
//! A dump loads over a thousand modules, so the list is a virtualised `uniform_list` (only visible rows are
//! laid out) — which needs equal-height rows: each one is a name line and a single truncated detail line.

use std::sync::Arc;

use dtb_ke_debugger::Outcome;
use dtb_ke_ui::components::icon::Icon;
use dtb_ke_ui::components::{Button, ButtonTone, Chip, ChipTone, Toggle};
use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::base::Scrollbar;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, SharedString, Styled,
    UniformListScrollHandle, div, px, uniform_list,
};

use super::DebuggerWindow;
use super::widgets::{self, tint};

const ROW_HEIGHT: f32 = 60.;

/// One module, digested to display strings once (not on every frame).
#[derive(Clone)]
struct Row {
    name: String,
    /// `0x<base> · <debug id>`
    id: String,
    chip: &'static str,
    tone: ChipTone,
    /// Where the file came from and its path, or what was tried — one line, truncated with an ellipsis.
    detail: String,
}

pub struct State {
    scroll: UniformListScrollHandle,
    /// Hide modules that are part of the OS.
    hide_system: bool,
    /// The rows for `key` = (which analysis, hide flag); rebuilt only when either changes.
    rows: Arc<[Row]>,
    key: Option<(usize, bool)>,
}

impl State {
    pub fn set_hide_system(&mut self, hide: bool) {
        self.hide_system = hide;
    }
}

impl Default for State {
    fn default() -> Self {
        Self {
            scroll: UniformListScrollHandle::new(),
            hide_system: true,
            rows: Arc::from(Vec::new()),
            key: None,
        }
    }
}

impl DebuggerWindow {
    pub(super) fn render_symbols(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.theme().color;
        let mono = cx.theme().skin.mono_font_family();
        let Some(analysis) = self.analysis().cloned() else {
            return div().into_any_element();
        };
        let hide = self.symbols.hide_system;
        let me = cx.entity();

        let key = (Arc::as_ptr(&analysis) as usize, hide);
        if self.symbols.key != Some(key) {
            let mut modules: Vec<_> = analysis
                .resolution
                .modules
                .iter()
                .filter(|m| !(hide && m.module.is_system()))
                .collect();
            // Found first; the dump's own order otherwise keeps the main module on top.
            modules.sort_by_key(|m| matches!(m.outcome, Outcome::Missing { .. }));
            let rows: Vec<Row> = modules
                .into_iter()
                .map(|m| {
                    let (chip, tone, detail) = match &m.outcome {
                        Outcome::Found(f) => (
                            if f.has_debug_info {
                                "debug info"
                            } else {
                                "symbol table only"
                            },
                            if f.has_debug_info {
                                ChipTone::Ok
                            } else {
                                ChipTone::Warn
                            },
                            if f.in_dyld_cache {
                                f.origin.clone()
                            } else {
                                format!("{} — {}", f.origin, f.path.display())
                            },
                        ),
                        Outcome::Missing { tried } => {
                            ("missing", ChipTone::Critical, tried.join("  ·  "))
                        }
                    };
                    let id = m
                        .module
                        .debug_id
                        .map(|d| d.to_string())
                        .unwrap_or_else(|| "no debug id".into());
                    Row {
                        name: m.module.short_name().to_owned(),
                        id: format!("{:#x} · {id}", m.module.base),
                        chip,
                        tone,
                        detail,
                    }
                })
                .collect();
            self.symbols.rows = Arc::from(rows);
            self.symbols.key = Some(key);
        }
        let rows = self.symbols.rows.clone();

        let bar = div()
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .h(px(36.))
            .border_b_1()
            .border_color(c.border)
            .child(
                Toggle::new("hide-system", hide).on_change(move |next, _w, cx| {
                    me.update(cx, |this, cx| {
                        this.symbols.set_hide_system(next);
                        cx.notify();
                    });
                }),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child("Hide OS modules"),
            )
            .child(
                div()
                    .text_size(px(12.))
                    .text_color(c.muted_foreground)
                    .child(SharedString::from(format!("· {} shown", rows.len()))),
            )
            .child(div().flex_1())
            .child(
                Button::new("re-resolve", "Resolve again")
                    .small()
                    .tone(ButtonTone::Secondary)
                    .leading_icon(Icon::RotateCw)
                    .on_click(cx.listener(|this, _, _, cx| this.analyze(cx))),
            );

        let count = rows.len();
        let list = uniform_list("modules", count, move |range, _w, _cx| {
            range
                .filter_map(|ix| rows.get(ix).cloned().map(|r| (ix, r)))
                .map(|(ix, r)| {
                    // Left: the module and where its file came from. Right: its address / debug id with the
                    // status pill underneath.
                    div()
                        .id(("module", ix))
                        .flex()
                        .items_center()
                        .gap(px(16.))
                        .w_full()
                        .h(px(ROW_HEIGHT))
                        .px(px(14.))
                        .border_b_1()
                        .border_color(tint(c.border, 0.6))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_1()
                                .min_w(px(0.))
                                .gap(px(5.))
                                .child(
                                    div()
                                        .text_size(px(13.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(SharedString::from(r.name)),
                                )
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(11.))
                                        .text_color(c.muted_foreground)
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .child(SharedString::from(r.detail)),
                                ),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .flex_none()
                                .items_end()
                                .gap(px(5.))
                                .child(
                                    div()
                                        .font_family(mono.clone())
                                        .text_size(px(10.5))
                                        .text_color(c.muted_foreground)
                                        .child(SharedString::from(r.id)),
                                )
                                .child(Chip::new(r.chip).tone(r.tone)),
                        )
                })
                .collect::<Vec<_>>()
        })
        .track_scroll(&self.symbols.scroll)
        .size_full();

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h(px(0.))
            .child(bar)
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h(px(0.))
                    .child(list)
                    .child(Scrollbar::vertical(&self.symbols.scroll)),
            )
            .child(widgets::placeholder(
                "Missing modules: drop debug files (dSYM, PDB, binary) or whole folders in, or add them with \"Debug files …\" — the dump is then resolved again.",
                &c,
            ))
            .into_any_element()
    }
}
