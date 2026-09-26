//! "Build": the build-info user stream, plus the dump's own facts.

use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::base::Scrollbar;
use gpui_kit::{
    AnyElement, Context, InteractiveElement, IntoElement, ParentElement, ScrollHandle,
    StatefulInteractiveElement, Styled, div, px,
};

use super::DebuggerWindow;
use super::widgets::{self, ListStyle, listing_rows, section_title};

#[derive(Default)]
pub struct State {
    scroll: ScrollHandle,
}

impl DebuggerWindow {
    pub(super) fn render_info(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let c = cx.theme().color;
        let Some(session) = &self.session else {
            return div().into_any_element();
        };

        let mut rows = vec![("File".to_owned(), session.path.display().to_string())];
        if let Ok(meta) = std::fs::metadata(&session.path) {
            rows.push(("Size".into(), format!("{} bytes", meta.len())));
        }
        rows.push(("Modules".into(), session.opened.modules.len().to_string()));

        // One scroll area for the whole tab (the dump facts and the build pairs scroll together).
        let mut body = div()
            .id("build-scroll")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.info.scroll)
            .child(section_title("Dump", &c))
            .child(listing_rows(rows, ListStyle::Inline, cx))
            .child(section_title("Build (from the dump)", &c));
        body = match &session.opened.build {
            Some(b) => {
                let mut pairs: Vec<(String, String)> = b.pairs.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
                if b.dirty() {
                    pairs.insert(0, ("Note".into(), "Built with uncommitted changes — the commit does not describe the source exactly.".into()));
                }
                body.child(listing_rows(pairs, ListStyle::Inline, cx))
            }
            None => body.child(widgets::placeholder(
                "This dump has no build-info stream (an older build, or a foreign dump). Debug files have to be supplied by hand.",
                &c,
            )),
        };

        // What the crashed machine could tell about its system frames (`dtb-ke-crash::syshints`).
        if let Some(h) = &session.opened.hints {
            use dtb_ke_crash::syshints::Quality;
            let quality = match h.quality {
                Quality::Exact => "exact (full local symbols)",
                Quality::Approximate => "approximate (exported symbols only)",
            };
            let rows = vec![
                ("producer".to_owned(), h.producer.clone()),
                ("quality".to_owned(), quality.to_owned()),
                (
                    "complete".to_owned(),
                    if h.complete {
                        "yes".to_owned()
                    } else {
                        format!("no — {}", h.incomplete_reason)
                    },
                ),
                (
                    "frames".to_owned(),
                    format!(
                        "{} of {} system frames named",
                        h.frames_resolved, h.frames_total
                    ),
                ),
                ("os build".to_owned(), h.os_build.clone()),
                ("functions".to_owned(), h.entries.len().to_string()),
            ];
            body = body
                .child(section_title(
                    "System symbols (named on the crashing machine)",
                    &c,
                ))
                .child(listing_rows(rows, ListStyle::Inline, cx));
        }

        div()
            .relative()
            .flex_1()
            .min_h(px(0.))
            .child(body)
            .child(Scrollbar::vertical(&self.info.scroll))
            .into_any_element()
    }
}
