//! A debug view exercising every primitive in the current skin.
//!
//! `DTB_KE_GALLERY=1 cargo run -p dtb-ke-ui` opens this instead of the app.

use gpui_kit::{
    App, AppContext, Bounds, Context, Entity, IntoElement, ParentElement, Render, Size, Styled,
    TitlebarOptions, Window, WindowBounds, WindowOptions, div, px,
};
use gpui_kit::base::input::InputState;

use crate::components::button::{Button, ButtonTone};
use crate::components::chip::{Chip, ChipTone};
use crate::components::field::Field;
use crate::components::icon::Icon;
use crate::components::segmented::Segmented;
use crate::theme::{ActiveTheme, Skin};

pub struct Gallery {
    name_field: Entity<InputState>,
    search_field: Entity<InputState>,
    disabled_field: Entity<InputState>,
    phase: usize,
}

impl Gallery {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            name_field: cx.new(|cx| InputState::new(window, cx).placeholder("Kampfrichtername")),
            search_field: cx.new(|cx| InputState::new(window, cx).placeholder("Wettkampf suchen…")),
            disabled_field: cx.new(|cx| InputState::new(window, cx).placeholder("deaktiviert")),
            phase: 0,
        }
    }
}

impl Render for Gallery {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.color.muted_foreground;

        let heading = |text: &str| {
            div()
                .text_size(px(11.))
                .text_color(muted)
                .child(text.to_uppercase())
        };
        let section = |title: &str, row: gpui_kit::AnyElement| {
            div()
                .flex()
                .flex_col()
                .gap(px(8.))
                .child(heading(title))
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .items_center()
                        .gap(px(12.))
                        .child(row),
                )
        };

        let view = cx.entity().downgrade();

        div()
            .size_full()
            .bg(theme.color.background)
            .text_color(theme.color.foreground)
            .p(px(28.))
            .flex()
            .flex_col()
            .gap(px(22.))
            .overflow_hidden()
            .child(
                div()
                    .text_size(px(17.))
                    .child(format!("Komponenten — Skin {:?}", Skin::detect())),
            )
            .child(section(
                "Buttons",
                div()
                    .flex()
                    .items_center()
                    .gap(px(10.))
                    .child(Button::new("primary", "Vorschau").tone(ButtonTone::Primary))
                    .child(Button::new("secondary", "Export"))
                    .child(Button::new("ghost", "Abbrechen").tone(ButtonTone::Ghost))
                    .child(Button::new("danger", "Löschen").tone(ButtonTone::Danger))
                    .child(Button::new("disabled", "Deaktiviert").disabled(true))
                    .child(Button::icon("icon", Icon::Plus))
                    .child(Button::new("small", "Klein").small())
                    .into_any_element(),
            ))
            .child(section(
                "Chips",
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Chip::new("Cyr · Artistic").tone(ChipTone::Accent))
                    .child(
                        Chip::new("Gespeichert")
                            .tone(ChipTone::Ok)
                            .leading_icon(Icon::Check),
                    )
                    .child(
                        Chip::new("3 Konflikte")
                            .tone(ChipTone::Warn)
                            .leading_icon(Icon::Warning),
                    )
                    .child(Chip::new("Fehler").tone(ChipTone::Critical))
                    .child(Chip::new("Quali 08:00 · Finale 13:30"))
                    .into_any_element(),
            ))
            .child(section(
                "Segmented",
                Segmented::new("phase", ["Qualifikation", "Finale"], self.phase)
                    .on_select(move |i, _window, cx| {
                        view.update(cx, |this, cx| {
                            this.phase = i;
                            cx.notify();
                        })
                        .ok();
                    })
                    .into_any_element(),
            ))
            .child(section(
                "Fields",
                div()
                    .flex()
                    .flex_col()
                    .gap(px(10.))
                    .w(px(320.))
                    .child(Field::new("name", &self.name_field))
                    .child(Field::new("search", &self.search_field).leading_icon(Icon::Search))
                    .child(Field::new("disabled", &self.disabled_field).disabled(true))
                    .into_any_element(),
            ))
            .child(section(
                "Icons",
                div()
                    .flex()
                    .items_center()
                    .gap(px(14.))
                    .children(
                        Icon::ALL
                            .iter()
                            .map(|icon| icon.size(px(18.)).color(muted).into_any_element()),
                    )
                    .into_any_element(),
            ))
    }
}

/// Open the component gallery in its own window.
pub fn open_gallery_window(cx: &mut App) {
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::centered(
            None,
            Size::new(px(820.), px(720.)),
            cx,
        ))),
        titlebar: Some(TitlebarOptions {
            title: Some("DTB · Komponenten".into()),
            ..Default::default()
        }),
        ..Default::default()
    };
    cx.open_window(options, |window, cx| cx.new(|cx| Gallery::new(window, cx)))
        .expect("failed to open the gallery window");
}
