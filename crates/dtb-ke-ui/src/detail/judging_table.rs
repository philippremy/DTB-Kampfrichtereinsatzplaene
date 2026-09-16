//! One judging-table card: editable label, discipline chip, a labelled judge
//! field per role. Selecting a card enables the detail toolbar's duplicate /
//! delete actions.
//!
//! Each judge field is a `gpui_base` `InputState`, seeded from the current
//! [`JudgingTableEditor`] value. On every change the card rebuilds the whole
//! `JudgingTableKindDTO` from the current field values and writes it back, which
//! propagates through `RoundEditor` → `CompetitionDocument` → autosave.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, AppContext, Bounds, Context, Entity, EntityId, Focusable as _, InteractiveElement,
    IntoElement, MouseButton, MouseDownEvent, ParentElement, Pixels, Point, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, anchored, canvas, deferred, div,
    point, prelude::FluentBuilder, px,
};
use gpui_base::input::{InputEvent, InputState};

use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuHandler, ContextMenuItem, context_menu};
use crate::components::field::Field;
use crate::components::icon::Icon;
use crate::i18n::ActiveLocale;
use crate::model::roles::{self, Discipline};
use crate::store::JudgingTableEditor;
use crate::theme::ActiveTheme;

pub type SelectHandler = Rc<dyn Fn(&mut App)>;
/// Invoked from the card's delete button (or context menu); `DetailView` runs
/// the confirm prompt.
pub type DeleteHandler = Rc<dyn Fn(&mut Window, &mut App)>;
/// Invoked from the card's context menu.
pub type DuplicateHandler = Rc<dyn Fn(&mut Window, &mut App)>;
/// `(dragged editor id, drop-target editor id)` — move the first before/at the second.
pub type ReorderHandler = Rc<dyn Fn(EntityId, EntityId, &mut Window, &mut App)>;

/// Drag payload for reordering a judging table — also renders its own drag ghost.
#[derive(Clone)]
pub struct DragTable {
    editor_id: EntityId,
    label: SharedString,
}

impl Render for DragTable {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = cx.theme().color;
        div()
            .px(px(10.))
            .py(px(5.))
            .rounded(cx.theme().skin.radius_control_px())
            .border_1()
            .border_color(c.primary)
            .bg(c.surface)
            .text_color(c.foreground)
            .text_size(px(12.5))
            .shadow_lg()
            .child(self.label.clone())
    }
}

pub struct JudgingTableCard {
    editor: Entity<JudgingTableEditor>,
    label_input: Entity<InputState>,
    slots: Vec<(&'static str, Entity<InputState>)>,
    selected: bool,
    /// Role labels whose judge is also assigned elsewhere in the phase.
    conflict_roles: Vec<&'static str>,
    /// Whether the discipline picker popover is showing.
    discipline_open: bool,
    /// Absolute bounds of the discipline trigger, captured during prepaint so
    /// the popover can float above the scroll container that would clip it.
    discipline_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// The card's right-click context menu, anchored at this window-absolute
    /// point — `None` unless it is currently open.
    context_menu: Option<Point<Pixels>>,
    on_select: SelectHandler,
    on_delete: DeleteHandler,
    on_duplicate: DuplicateHandler,
    on_reorder: ReorderHandler,
    _subs: Vec<Subscription>,
}

impl JudgingTableCard {
    pub fn new(
        editor: Entity<JudgingTableEditor>,
        on_select: SelectHandler,
        on_delete: DeleteHandler,
        on_duplicate: DuplicateHandler,
        on_reorder: ReorderHandler,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let table = editor.read(cx).table().clone();

        let placeholder = cx.t("detail.wizard.name-placeholder");
        let label_input = cx.new(|cx| {
            InputState::new(window, cx)
                .default_value(table.label.clone())
                .placeholder(placeholder)
        });

        let slots = build_slots(&table.kind, window, cx);

        let mut this = Self {
            editor,
            label_input,
            slots,
            selected: false,
            conflict_roles: Vec::new(),
            discipline_open: false,
            discipline_bounds: Rc::new(Cell::new(None)),
            context_menu: None,
            on_select,
            on_delete,
            on_duplicate,
            on_reorder,
            _subs: Vec::new(),
        };
        this.wire_subs(window, cx);
        this
    }

    /// Rebuild `_subs` so it tracks the current label field + judge fields.
    /// Called on construction and after the discipline (and thus the slot set)
    /// changes.
    fn wire_subs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let mut subs = vec![cx.subscribe_in(
            &self.label_input,
            window,
            |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Change => {
                    let value = this.label_input.read(cx).value().to_string();
                    this.editor
                        .update(cx, |editor, cx| editor.set_label(value, cx));
                }
                InputEvent::PressEnter { .. } => window.blur(cx),
                _ => {}
            },
        )];
        for (_, input) in &self.slots {
            subs.push(
                cx.subscribe_in(
                    input,
                    window,
                    |this, _, event: &InputEvent, window, cx| match event {
                        InputEvent::Change => this.write_back(cx),
                        InputEvent::PressEnter { .. } => window.blur(cx),
                        _ => {}
                    },
                ),
            );
        }
        self._subs = subs;
    }

    /// Re-cast the table to `target`, keeping every judge whose role survives
    /// the change and dropping the rest, then rebuild the judge fields.
    fn change_discipline(
        &mut self,
        target: Discipline,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.discipline_open = false;
        let current = self.editor.read(cx).table().kind.clone();
        if Discipline::of(&current) == target {
            cx.notify();
            return;
        }
        let kind = roles::change_discipline(&current, target);
        self.editor
            .update(cx, |editor, cx| editor.set_kind(kind.clone(), cx));
        self.slots = build_slots(&kind, window, cx);
        self.wire_subs(window, cx);
        cx.notify();
    }

    pub fn editor_id(&self) -> EntityId {
        self.editor.entity_id()
    }

    pub fn table_editor(&self) -> Entity<JudgingTableEditor> {
        self.editor.clone()
    }

    pub fn set_selected(&mut self, selected: bool, cx: &mut Context<Self>) {
        if self.selected != selected {
            self.selected = selected;
            cx.notify();
        }
    }

    pub fn set_conflict_roles(&mut self, roles: Vec<&'static str>, cx: &mut Context<Self>) {
        if self.conflict_roles != roles {
            self.conflict_roles = roles;
            cx.notify();
        }
    }

    /// The label field + every judge field, as plain handles — captured into the
    /// card's click closures so they can check focus without re-entering the
    /// card's own `update` (which `select_table` would, via `card.read`).
    fn input_handles(&self) -> Vec<Entity<InputState>> {
        std::iter::once(self.label_input.clone())
            .chain(self.slots.iter().map(|(_, input)| input.clone()))
            .collect()
    }

    fn write_back(&self, cx: &mut Context<Self>) {
        let values: Vec<String> = self
            .slots
            .iter()
            .map(|(_, input)| input.read(cx).value().to_string())
            .collect();
        let kind = roles::apply(&self.editor.read(cx).table().kind, &values);
        self.editor
            .update(cx, |editor, cx| editor.set_kind(kind, cx));
    }

    /// The discipline pill in the card header — a trigger that opens a popover
    /// list of every discipline. Painted via `deferred` at the trigger's
    /// absolute position so it floats above the detail scroll container.
    fn discipline_selector(&self, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.discipline_open;
        let current = Discipline::of(&self.editor.read(cx).table().kind);

        let list = open
            .then(|| self.discipline_bounds.get())
            .flatten()
            .map(|b| {
                let anchor = point(b.origin.x, b.origin.y + b.size.height + px(4.));
                let mut col = div()
                    .id("discipline-list")
                    .min_w(b.size.width.max(px(190.)))
                    .flex()
                    .flex_col()
                    .py(px(2.))
                    .rounded(radius)
                    .border_1()
                    .border_color(c.border)
                    .bg(c.surface)
                    .shadow_lg()
                    .occlude();
                for (i, d) in Discipline::ALL.into_iter().enumerate() {
                    let selected = d == current;
                    col = col.child(
                        div()
                            .id(("discipline-opt", i))
                            .px(px(9.))
                            .py(px(5.))
                            .text_size(px(12.5))
                            .cursor_pointer()
                            .when(selected, |el| el.text_color(c.primary))
                            .hover(|el| el.bg(c.accent_soft))
                            .on_mouse_down(
                                MouseButton::Left,
                                cx.listener(move |this, _, window, cx| {
                                    this.change_discipline(d, window, cx)
                                }),
                            )
                            .child(cx.t(d.label_key())),
                    );
                }
                // Above the scroll container (and any dialog) — see `meta_dialog`.
                deferred(anchored().position(anchor).snap_to_window().child(col))
                    .with_priority(gpui_base::POPUP_PRIORITY)
            });

        let capture = self.discipline_bounds.clone();

        div()
            .id("discipline-select")
            .relative()
            .flex()
            .flex_col()
            .on_mouse_down_out(cx.listener(|this, _, _w, cx| {
                if this.discipline_open {
                    this.discipline_open = false;
                    cx.notify();
                }
            }))
            .child(
                canvas(
                    move |bounds, window, _cx| {
                        if capture.get() != Some(bounds) {
                            capture.set(Some(bounds));
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div()
                    .id("discipline-trigger")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(24.))
                    .px(px(8.))
                    .rounded(radius)
                    .border_1()
                    .border_color(if open { c.primary } else { c.border })
                    .bg(c.accent_soft)
                    .text_color(c.primary)
                    .text_size(px(11.))
                    .cursor_pointer()
                    // Toggling the picker must not also select the card (the
                    // card's own `on_click` runs after this one bubbles).
                    .on_click(cx.listener(|this, _, _w, cx| {
                        cx.stop_propagation();
                        this.discipline_open = !this.discipline_open;
                        cx.notify();
                    }))
                    .child(cx.t(current.label_key()))
                    .child(Icon::ChevronDown.size(px(12.)).color(c.primary)),
            )
            .children(list)
    }

    fn open_context_menu(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.context_menu = Some(position);
        cx.notify();
    }

    fn close_context_menu(&mut self, cx: &mut Context<Self>) {
        if self.context_menu.take().is_some() {
            cx.notify();
        }
    }

    /// The card's right-click context menu (delete / duplicate) — `None`
    /// unless it is currently open.
    fn context_menu_layer(&self, cx: &mut Context<Self>) -> Option<gpui::AnyElement> {
        let position = self.context_menu?;
        let weak = cx.weak_entity();

        let on_dismiss: ContextMenuHandler = {
            let weak = weak.clone();
            Rc::new(move |_window: &mut Window, cx: &mut App| {
                weak.update(cx, |this, cx| this.close_context_menu(cx)).ok();
            })
        };

        let items = vec![
            ContextMenuItem::new(
                "ctx-duplicate",
                cx.t("detail.toolbar-duplicate"),
                Icon::Copy,
                {
                    let on_duplicate = self.on_duplicate.clone();
                    move |window: &mut Window, cx: &mut App| on_duplicate(window, cx)
                },
            ),
            ContextMenuItem::new("ctx-delete", cx.t("detail.toolbar-delete"), Icon::Trash, {
                let on_delete = self.on_delete.clone();
                move |window: &mut Window, cx: &mut App| on_delete(window, cx)
            })
            .danger()
            .separated(),
        ];

        Some(context_menu(
            "table-context-menu",
            position,
            items,
            on_dismiss,
            cx,
        ))
    }
}

/// One `InputState` per judge slot of `kind`, seeded from its current value.
fn build_slots(
    kind: &dtb_ke_types::JudgingTableKindDTO,
    window: &mut Window,
    cx: &mut Context<JudgingTableCard>,
) -> Vec<(&'static str, Entity<InputState>)> {
    roles::slots(kind)
        .into_iter()
        .map(|slot| {
            let input = cx.new(|cx| InputState::new(window, cx).default_value(slot.value));
            (slot.label, input)
        })
        .collect()
}

/// Whether any of `inputs` currently holds keyboard focus.
fn any_focused(inputs: &[Entity<InputState>], window: &Window, cx: &App) -> bool {
    inputs
        .iter()
        .any(|input| input.read(cx).focus_handle(cx).is_focused(window))
}

impl Render for JudgingTableCard {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let discipline_selector = self.discipline_selector(cx);
        let context_menu_layer = self.context_menu_layer(cx);
        let theme = cx.theme();
        let c = &theme.color;
        let editor_id = self.editor.entity_id();
        let table = self.editor.read(cx).table();
        let drag_label: SharedString = if table.label.trim().is_empty() {
            cx.t("detail.wizard.fallback-name")
        } else {
            table.label.clone().into()
        };
        let selected = self.selected;
        let conflicted = !self.conflict_roles.is_empty();

        let on_select = self.on_select.clone();
        let on_reorder = self.on_reorder.clone();
        let click_inputs = self.input_handles();
        let out_inputs = click_inputs.clone();

        div()
            .id(("table-card", editor_id))
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(12.))
            .rounded(theme.skin.radius_lg_px())
            .border_1()
            .border_color(if conflicted {
                c.warn
            } else if selected {
                c.primary
            } else {
                c.border
            })
            .bg(c.surface)
            .when(selected, |el| el.bg(c.accent_soft))
            // Select on a click on the card itself — not one that landed in (and
            // focused) one of its text fields. A plain closure, *not*
            // `cx.listener`: `on_select` calls back into `DetailView::select_table`
            // which reads this card, which panics if we're inside `card.update`.
            .on_click(move |_, window, cx| {
                if !any_focused(&click_inputs, window, cx) {
                    on_select(cx);
                }
            })
            .on_mouse_down_out(move |_, window, cx| {
                if any_focused(&out_inputs, window, cx) {
                    window.blur(cx);
                }
            })
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, _window, cx| {
                    this.open_context_menu(event.position, cx);
                }),
            )
            // Drop target for a reordering drag.
            .on_drop(move |dragged: &DragTable, window, cx| {
                on_reorder(dragged.editor_id, editor_id, window, cx);
            })
            .drag_over::<DragTable>(|style, _dragged, _window, cx| {
                style
                    .border_t(px(2.))
                    .border_color(cx.theme().color.primary)
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        div()
                            .id("drag-handle")
                            .flex_none()
                            .cursor_move()
                            .child(Icon::Grip.size(px(14.)).color(c.muted_foreground))
                            .on_drag(
                                DragTable {
                                    editor_id,
                                    label: drag_label,
                                },
                                |dragged: &DragTable, _pos, _window, cx| {
                                    cx.new(|_| dragged.clone())
                                },
                            ),
                    )
                    .child(
                        div()
                            .flex_1()
                            .child(Field::new("table-label", &self.label_input)),
                    )
                    .child(discipline_selector)
                    .child(Button::icon("del-table", Icon::Trash).small().on_click(
                        cx.listener(|this, _, window, cx| (this.on_delete.clone())(window, cx)),
                    )),
            )
            .child(
                div()
                    .flex()
                    .flex_wrap()
                    .gap(px(8.))
                    .children(self.slots.iter().map(|(label, input)| {
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(3.))
                            .w(px(148.))
                            .child(
                                div()
                                    .text_size(px(9.))
                                    .text_color(c.muted_foreground)
                                    .child(*label),
                            )
                            .child(
                                Field::new(*label, input)
                                    .invalid(self.conflict_roles.contains(label)),
                            )
                    })),
            )
            .children(context_menu_layer)
    }
}
