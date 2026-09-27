//! The symbol server in the UI: where to fetch debug files that are not on this machine.
//!
//! An address + read-token panel on the Symbols tab (persisted, see `server_settings`), and a hint bar — like the
//! system-symbol one — that appears when modules in the stack traces have no real debug info (the crashed build's
//! own stripped executable gives a bare symbol table), offering to set the server up or explaining why it had nothing.

use std::collections::BTreeSet;

use dtb_ke_debugger::process::Analysis;
use dtb_ke_debugger::quality::Quality;
use dtb_ke_debugger::remote::{ServerConfig, ServerHandle, check_server};
use dtb_ke_debugger::server_settings::{self, Stored};
use dtb_ke_ui::components::icon::Icon;
use dtb_ke_ui::components::{Button, ButtonTone, Field};
use dtb_ke_ui::theme::ActiveTheme;
use gpui_kit::base::input::{InputEvent, InputState};
use gpui_kit::{
    AppContext as _, Context, Entity, Focusable as _, IntoElement, ParentElement, PromptButton,
    PromptLevel, SharedString, Styled, Subscription, Window, div, prelude::FluentBuilder, px,
};

use super::widgets;
use super::{DebuggerWindow, Phase, Tab};
use crate::tokio_bridge::Tokio;

pub enum Status {
    Idle,
    Testing,
    Ok(String),
    Failed(String),
}

/// Modules in the stack traces that have no real debug info, and whether a server was there to ask.
pub struct ServerHint {
    pub modules: Vec<String>,
    /// Every one of them at least has function names (a symbol table) — otherwise some are fully stripped.
    pub all_named: bool,
    pub configured: bool,
    /// What the server side said for the first of them (`symbol server (…): not found`, an HTTP error …).
    pub why: Option<String>,
}

pub struct ServerUi {
    /// Shared with the resolver's `SymbolServerSource`: setting it reconfigures the chain without rebuilding it.
    pub handle: ServerHandle,
    pub url: Entity<InputState>,
    pub token: Entity<InputState>,
    pub status: Status,
    pub hint: Option<ServerHint>,
    pub hint_dismissed: bool,
    _subs: Vec<Subscription>,
}

impl ServerUi {
    /// Loads the stored server (else the `DTB_KE_SYMBOL_*` environment) into the handle and the two fields.
    pub fn new(window: &mut Window, cx: &mut Context<DebuggerWindow>) -> Self {
        let initial =
            server_settings::initial(&server_settings::load(&server_settings::config_dir()));
        let handle = ServerHandle::default();
        handle.set(server_settings::to_config(&initial));

        let url = cx.new(|cx| {
            let mut state = InputState::new(window, cx).placeholder("https://symbols.example.net");
            state.set_value(initial.url.clone(), window, cx);
            state
        });
        let token = cx.new(|cx| {
            let mut state = InputState::new(window, cx)
                .placeholder("Read token")
                .masked(true);
            state.set_value(initial.token.clone(), window, cx);
            state
        });
        // Enter in either field saves, like a form.
        let subs = [&url, &token]
            .into_iter()
            .map(|field| {
                cx.subscribe_in(field, window, |this, _, event: &InputEvent, _window, cx| {
                    if let InputEvent::PressEnter { .. } = event {
                        this.save_server(cx);
                    }
                })
            })
            .collect();
        Self {
            handle,
            url,
            token,
            status: Status::Idle,
            hint: None,
            hint_dismissed: false,
            _subs: subs,
        }
    }
}

impl DebuggerWindow {
    fn typed_server(&self, cx: &Context<Self>) -> (String, String) {
        (
            self.server_ui.url.read(cx).value().to_string(),
            self.server_ui.token.read(cx).value().to_string(),
        )
    }

    /// Checks the typed address + token against the server without saving anything.
    pub(super) fn test_server(&mut self, cx: &mut Context<Self>) {
        let (url, token) = self.typed_server(cx);
        let config = match ServerConfig::parse(&url, &token) {
            Ok(Some(config)) => config,
            Ok(None) => {
                self.server_ui.status = Status::Failed("Enter the server address first.".into());
                cx.notify();
                return;
            }
            Err(message) => {
                self.server_ui.status = Status::Failed(message);
                cx.notify();
                return;
            }
        };
        self.server_ui.status = Status::Testing;
        cx.notify();
        let work = Tokio::spawn_result(cx, async move {
            check_server(&config).await.map_err(anyhow::Error::msg)
        });
        cx.spawn(async move |this, cx| {
            let result = work.await;
            this.update(cx, |this, cx| {
                this.server_ui.status = match result {
                    Ok(message) => Status::Ok(message),
                    Err(err) => Status::Failed(err.to_string()),
                };
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Applies and stores the typed server (an empty address removes it), then resolves the open dump again.
    pub(super) fn save_server(&mut self, cx: &mut Context<Self>) {
        let (url, token) = self.typed_server(cx);
        let config = match ServerConfig::parse(&url, &token) {
            Ok(config) => config,
            Err(message) => {
                self.server_ui.status = Status::Failed(message);
                cx.notify();
                return;
            }
        };
        let stored = config
            .as_ref()
            .map(|c| Stored {
                url: c.base.clone(),
                token: c.token.clone().unwrap_or_default(),
            })
            .unwrap_or_default();
        let saved = server_settings::save(&server_settings::config_dir(), &stored);
        match &config {
            Some(c) => log::info!("symbol server set to {}", c.base),
            None => log::info!("symbol server removed"),
        }
        self.server_ui.handle.set(config.clone());
        self.server_ui.status = match (saved, &config) {
            (Err(err), _) => {
                log::error!("cannot store the symbol server settings: {err}");
                Status::Failed(format!(
                    "Applied for this session, but could not be saved: {err}"
                ))
            }
            (Ok(()), Some(c)) => Status::Ok(format!("Saved — using {}.", c.base)),
            (Ok(()), None) => Status::Ok("No symbol server — removed.".into()),
        };
        self.server_ui.hint_dismissed = false;
        if self.session.is_some() {
            self.analyze(cx);
        } else {
            cx.notify();
        }
    }

    /// File ▸ Clear Symbol Cache …: says what is in the cache, asks, empties it, reports what was freed. The next
    /// resolve downloads what it needs from the server again.
    pub(super) fn clear_symbol_cache(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let cache = self.symbol_cache.clone();
        let (files, bytes) = cache.usage();
        let location = cache.root().display().to_string();
        if files == 0 {
            let done = window.prompt(
                PromptLevel::Info,
                "The symbol cache is empty",
                Some(&format!(
                    "Nothing downloaded from the symbol server is stored in {location}."
                )),
                &[PromptButton::Cancel("OK".into())],
                cx,
            );
            cx.spawn(async move |_, _| {
                let _ = done.await;
            })
            .detach();
            return;
        }
        let detail = format!(
            "This deletes {files} downloaded debug file{} ({}) from {location}. They are fetched from the symbol server \
             again the next time a crash report needs them.",
            if files == 1 { "" } else { "s" },
            human_bytes(bytes),
        );
        let answer = window.prompt(
            PromptLevel::Warning,
            "Clear the symbol cache?",
            Some(&detail),
            &[
                PromptButton::new("Clear"),
                PromptButton::Cancel("Cancel".into()),
            ],
            cx,
        );
        cx.spawn_in(window, async move |this, cx| {
            if answer.await.unwrap_or(1) != 0 {
                return;
            }
            let (title, message, level) = match cache.clear() {
                Ok((files, bytes)) => {
                    log::info!(
                        "symbol cache cleared: {files} file(s), {} freed",
                        human_bytes(bytes)
                    );
                    (
                        "Symbol cache cleared".to_owned(),
                        format!(
                            "Deleted {files} file{} ({}).",
                            if files == 1 { "" } else { "s" },
                            human_bytes(bytes)
                        ),
                        PromptLevel::Info,
                    )
                }
                Err(err) => {
                    log::error!("cannot clear the symbol cache: {err}");
                    (
                        "Could not clear the symbol cache".to_owned(),
                        err.to_string(),
                        PromptLevel::Critical,
                    )
                }
            };
            this.update_in(cx, |_, window, cx| {
                let done = window.prompt(
                    level,
                    &title,
                    Some(&message),
                    &[PromptButton::Cancel("OK".into())],
                    cx,
                );
                cx.spawn(async move |_, _| {
                    let _ = done.await;
                })
                .detach();
            })
            .ok();
        })
        .detach();
    }

    /// Jumps to the server settings (from the hint bar) and puts the cursor in the address field.
    pub(super) fn show_server_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = Tab::Symbols;
        let handle = self.server_ui.url.read(cx).focus_handle(cx);
        window.focus(&handle, cx);
        cx.notify();
    }

    /// Non-OS modules that some stack frame sits in but that have only a symbol table (or nothing).
    pub(super) fn server_hint_for(&self, analysis: &Analysis) -> Option<ServerHint> {
        use minidump::Module;
        let session = self.session.as_ref()?;
        let mut names = BTreeSet::new();
        let mut all_named = true;
        let mut why = None;
        for frame in analysis.state.threads.iter().flat_map(|t| &t.frames) {
            let Some(module) = &frame.module else {
                continue;
            };
            let base = module.base_address();
            let Some(resolution) = analysis.resolution.for_module(base) else {
                continue;
            };
            if resolution.module.is_system() {
                continue;
            }
            // By what the module's frames actually got: a debug-map executable has lines although its file has no DWARF,
            // and a stripped one has synthesized `fun_…` names although its file lists a few symbols.
            let quality = analysis.quality(resolution);
            if quality == Quality::DebugInfo {
                continue;
            }
            all_named &= quality.has_names();
            if why.is_none() {
                why = resolution
                    .notes
                    .iter()
                    .find(|n| n.starts_with("symbol server"))
                    .cloned();
            }
            names.insert(
                session
                    .names
                    .get(&base)
                    .cloned()
                    .unwrap_or_else(|| module.code_file().into_owned()),
            );
        }
        (!names.is_empty()).then(|| ServerHint {
            modules: names.into_iter().collect(),
            all_named,
            configured: self.server_ui.handle.get().is_some(),
            why,
        })
    }

    pub(super) fn server_hint_available(&self) -> bool {
        matches!(self.phase, Phase::Ready) && self.server_ui.hint.is_some()
    }

    /// The address + token panel at the top of the Symbols tab.
    pub(super) fn server_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let testing = matches!(self.server_ui.status, Status::Testing);
        let status = match &self.server_ui.status {
            Status::Idle => None,
            Status::Testing => Some(("Testing …".to_owned(), c.muted_foreground)),
            Status::Ok(message) => Some((message.clone(), c.ok)),
            Status::Failed(message) => Some((message.clone(), c.critical)),
        };
        div()
            .flex()
            .flex_col()
            .flex_none()
            .gap(px(6.))
            .px(px(14.))
            .py(px(10.))
            .border_b_1()
            .border_color(c.border)
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .child(div().text_size(px(13.)).child("Symbol server"))
                    .child(
                        div()
                            .text_size(px(11.5))
                            .text_color(c.muted_foreground)
                            .child(
                                "Debug files that are not on this machine are fetched from here by debug id. \
                                 Saved in your user config folder (the token as plain text).",
                            ),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(div().flex_1().min_w(px(0.)).child(Field::new("server-url", &self.server_ui.url).height(px(28.))))
                    .child(div().w(px(240.)).child(Field::new("server-token", &self.server_ui.token).height(px(28.))))
                    .child(
                        Button::new("server-test", "Test")
                            .small()
                            .tone(ButtonTone::Secondary)
                            .disabled(testing)
                            .on_click(cx.listener(|this, _, _, cx| this.test_server(cx))),
                    )
                    .child(
                        Button::new("server-save", "Save & resolve again")
                            .small()
                            .tone(ButtonTone::Primary)
                            .on_click(cx.listener(|this, _, _, cx| this.save_server(cx))),
                    ),
            )
            .when_some(status, |el, (text, color)| {
                el.child(div().text_size(px(12.)).text_color(color).child(SharedString::from(text)))
            })
    }

    /// Modules with only a symbol table (or nothing): offers the server setup, or says why the server had nothing.
    pub(super) fn server_hint_bar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        let hint = self.server_ui.hint.as_ref();
        let (configured, modules, why, all_named) = hint
            .map(|h| (h.configured, h.modules.clone(), h.why.clone(), h.all_named))
            .unwrap_or_default();
        let count = modules.len();
        let mut sample = modules
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
        if count > 3 {
            sample.push_str(", …");
        }
        let message = if configured {
            format!(
                "The symbol server had no usable debug file for {sample}{}. Was this build uploaded, and are the address \
                 and token right?",
                why.map(|w| format!(" ({w})")).unwrap_or_default()
            )
        } else {
            let effect = if all_named {
                "their frames show function names but no source lines"
            } else {
                "their frames have no real function names (only synthesized fun_<address> ones) and no source lines"
            };
            format!(
                "{count} module{} in the stack traces {} no debug info ({sample}) — {effect}. Set up your symbol server \
                 to fetch the build's debug files by debug id.",
                if count == 1 { "" } else { "s" },
                if count == 1 { "has" } else { "have" },
            )
        };
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
                Button::new(
                    "server-hint-open",
                    if configured {
                        "Server settings …"
                    } else {
                        "Set up server …"
                    },
                )
                .small()
                .tone(ButtonTone::Secondary)
                .leading_icon(Icon::Settings)
                .on_click(cx.listener(|this, _, window, cx| this.show_server_settings(window, cx))),
            )
            .child(
                Button::new("server-hint-dismiss", "Dismiss")
                    .small()
                    .tone(ButtonTone::Ghost)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.server_ui.hint_dismissed = true;
                        cx.notify();
                    })),
            )
    }
}

/// `1.0 GB`, `312 MB`, `4.5 KB` — decimal units, one decimal above MB.
fn human_bytes(bytes: u64) -> String {
    const UNITS: [(&str, f64); 3] = [("GB", 1e9), ("MB", 1e6), ("KB", 1e3)];
    for (unit, size) in UNITS {
        if bytes as f64 >= size {
            let value = bytes as f64 / size;
            return if value >= 100.0 {
                format!("{value:.0} {unit}")
            } else {
                format!("{value:.1} {unit}")
            };
        }
    }
    format!("{bytes} B")
}

#[cfg(test)]
mod tests {
    use super::human_bytes;

    #[test]
    fn sizes_read_naturally() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(999), "999 B");
        assert_eq!(human_bytes(4_500), "4.5 KB");
        assert_eq!(human_bytes(312_000_000), "312 MB");
        assert_eq!(human_bytes(1_061_046_214), "1.1 GB");
    }
}
