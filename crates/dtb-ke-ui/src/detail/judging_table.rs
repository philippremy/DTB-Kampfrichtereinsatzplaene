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

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt, App, AppContext, Bounds, Context, CursorStyle, DragMoveEvent, Entity,
    EntityId, Focusable as _, Hsla, InteractiveElement, IntoElement, MouseButton, MouseDownEvent,
    ParentElement, Pixels, Point, Render, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, canvas, div, ease_out_quint, prelude::FluentBuilder, px,
};
use gpui_kit::base::input::{InputEvent, InputState};

use crate::components::button::Button;
use crate::components::context_menu::{ContextMenuHandler, ContextMenuItem, context_menu};
use crate::components::field::Field;
use crate::components::focus::field_border_color;
use crate::components::icon::Icon;
use crate::components::popover::PopoverAnchor;
use crate::i18n::ActiveLocale;
use crate::model::roles::{self, Discipline};
use crate::skin::glass::{self, GlassRole};
use crate::store::JudgingTableEditor;
use crate::theme::{ActiveTheme, Appearance};

pub type SelectHandler = Rc<dyn Fn(&mut App)>;
/// Invoked from the card's delete button (or context menu); `DetailView` runs
/// the confirm prompt.
pub type DeleteHandler = Rc<dyn Fn(&mut Window, &mut App)>;
/// Invoked from the card's context menu.
pub type DuplicateHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Every `DetailView`-supplied callback a card needs, bundled so `new` stays
/// under clippy's argument-count lint instead of taking several separate
/// ones. Reordering itself is handled by `DetailView` centrally (see
/// `phase_body`'s own `on_drag_move`/`on_drop`), not per-card — see that
/// method's doc comment for why.
pub struct CardHandlers {
    pub on_select: SelectHandler,
    pub on_delete: DeleteHandler,
    pub on_duplicate: DuplicateHandler,
}

/// Drag payload for reordering a judging table — also renders its own drag
/// ghost: a read-only snapshot of the card's actual content (not the live,
/// editable card itself — a drag ghost shouldn't be interactive), at reduced
/// opacity to read as "mid-drag".
#[derive(Clone)]
pub struct DragTable {
    editor_id: EntityId,
    label: SharedString,
    kind: dtb_ke_types::JudgingTableKindDTO,
    /// The real card's own last-measured width (captured via a canvas probe —
    /// see `JudgingTableCard`'s `width_bounds`), so the ghost matches its
    /// actual on-screen size instead of some guessed constant.
    width: Pixels,
}

impl DragTable {
    pub fn editor_id(&self) -> EntityId {
        self.editor_id
    }
}

impl Render for DragTable {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let c = theme.color;
        let radius = theme.skin.radius_lg_px();
        let field_radius = theme.skin.radius_control_px();
        let discipline = Discipline::of(&self.kind);

        div()
            .opacity(0.85)
            .flex()
            .flex_col()
            .gap(px(10.))
            .p(px(12.))
            .w(self.width)
            .rounded(radius)
            .border_1()
            .border_color(c.primary)
            .bg(c.surface)
            .text_color(c.foreground)
            .shadow_lg()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(Icon::Grip.size(px(14.)).color(c.muted_foreground))
                    .child(div().flex_1().text_size(px(13.)).child(self.label.clone()))
                    .child(
                        div()
                            .flex_none()
                            .flex()
                            .items_center()
                            .h(px(24.))
                            .px(px(8.))
                            .rounded(field_radius)
                            .bg(c.accent_soft)
                            .text_color(c.primary)
                            .text_size(px(11.))
                            .child(cx.t(discipline.label_key())),
                    ),
            )
            .child(div().flex().flex_wrap().gap(px(8.)).children(
                roles::slots(&self.kind).into_iter().map(|slot| {
                    div()
                        .flex()
                        .flex_col()
                        .gap(px(3.))
                        .w(px(148.))
                        .child(
                            div()
                                .text_size(px(9.))
                                .text_color(c.muted_foreground)
                                .child(slot.label),
                        )
                        .child(
                            div()
                                .h(px(30.))
                                .px(px(8.))
                                .flex()
                                .items_center()
                                .rounded(field_radius)
                                .border_1()
                                .border_color(c.border)
                                .bg(c.surface)
                                .text_color(c.foreground)
                                .text_size(px(13.))
                                .child(SharedString::from(slot.value)),
                        )
                }),
            ))
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
    discipline_bounds: PopoverAnchor,
    /// The card's own last-painted bounds, captured the same way as
    /// `discipline_bounds` — read at drag-start so the drag ghost (see
    /// `DragTable`) can be sized to match the real card's current width, and
    /// (see the same canvas probe in `render`) to detect a reorder-preview
    /// jump for the FLIP reflow animation below.
    card_bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    /// FLIP state for the reorder-reflow animation: `(offset, generation)`.
    /// `offset` is the vertical distance (in the direction *away from* the
    /// card's new position) to animate back down to zero from; `generation`
    /// changes every time a new jump is detected, forcing `with_animation`'s
    /// element-state (keyed by id, see `render`) to restart rather than
    /// resume an unrelated, possibly-already-finished tween.
    flip: Rc<Cell<(Pixels, u64)>>,
    /// The card's right-click context menu, anchored at this window-absolute
    /// point — `None` unless it is currently open.
    context_menu: Option<Point<Pixels>>,
    /// Whether *this* card is the source of the currently active reordering
    /// drag (tracked via `on_drag_move`, see `render`) — gated by
    /// `cx.has_active_drag()` at render time since nothing ever clears it
    /// back to `false` directly.
    dragging: bool,
    /// The detail scroll pane's own current window-relative visible bounds —
    /// a shared `Rc<Cell>` owned by `DetailView` and handed to every card, so
    /// each card's native glass region (see `render`) can clip itself to the
    /// pane exactly like gpui's own `overflow_y_scroll` clips the card's
    /// gpui content. `None` before the pane's own probe has painted once.
    viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
    on_select: SelectHandler,
    on_delete: DeleteHandler,
    on_duplicate: DuplicateHandler,
    _subs: Vec<Subscription>,
}

impl JudgingTableCard {
    pub fn new(
        editor: Entity<JudgingTableEditor>,
        handlers: CardHandlers,
        viewport: Rc<Cell<Option<Bounds<Pixels>>>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let CardHandlers {
            on_select,
            on_delete,
            on_duplicate,
        } = handlers;
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
            discipline_bounds: PopoverAnchor::new(),
            card_bounds: Rc::new(Cell::new(None)),
            flip: Rc::new(Cell::new((px(0.), 0))),
            context_menu: None,
            dragging: false,
            viewport,
            on_select,
            on_delete,
            on_duplicate,
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

    /// The card's own last-painted height — content-driven (the judging
    /// table's own discipline/slot count), not dependent on where in the
    /// list it currently renders, so `DetailView::phase_body` can use it to
    /// compute a live reorder preview from raw cursor position without
    /// needing to know anything about the *current* (possibly already
    /// reordered) on-screen layout.
    pub fn natural_height(&self) -> Option<Pixels> {
        self.card_bounds.get().map(|b| b.size.height)
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
    fn discipline_selector(
        &self,
        accent: Option<Hsla>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let c = cx.theme().color;
        let radius = cx.theme().skin.radius_control_px();
        let open = self.discipline_open;
        let current = Discipline::of(&self.editor.read(cx).table().kind);

        let list = open
            .then(|| self.discipline_bounds.get())
            .flatten()
            .map(|b| {
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
                col
            })
            // Above the scroll container (and any dialog) — see `meta_dialog`.
            .and_then(|col| self.discipline_bounds.float_below(px(4.), col));

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
            .child(self.discipline_bounds.probe())
            .child(
                div()
                    .id("discipline-trigger")
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .h(px(30.))
                    .px(px(8.))
                    .rounded(radius)
                    .border_1()
                    .border_color(field_border_color(open, accent, cx.theme()))
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
    fn context_menu_layer(&self, cx: &mut Context<Self>) -> Option<gpui_kit::AnyElement> {
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

/// A stable, per-table native glass region id — `editor_id` never changes for
/// as long as this judging table exists (see `JudgingTableCard::editor_id`).
fn glass_id(editor_id: EntityId) -> SharedString {
    SharedString::from(format!("table-card-{editor_id:?}"))
}

/// The id of that same card's [`GlassRole::CardOverlay`] — the selected/
/// conflicted accent panel layered above it.
fn overlay_glass_id(editor_id: EntityId) -> SharedString {
    SharedString::from(format!("table-card-{editor_id:?}-overlay"))
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
        let selected = self.selected;
        let conflicted = !self.conflict_roles.is_empty();
        // The card's own selected/conflicted colour, reused below for every
        // field's (and the discipline selector's) outline and role label —
        // not just the one field `Field::invalid` already outlines — so they
        // stay legible against the card's colour-tinted glass overlay (or,
        // on the fallback tier, its tinted `bg`). A scoped, owned copy of
        // `theme.color` (not `let theme = cx.theme()`'s live reference) so
        // it's ready before `discipline_selector` needs its own `&mut cx`.
        let accent = {
            let color = cx.theme().color;
            if conflicted {
                Some(color.warn)
            } else if selected {
                Some(color.primary)
            } else {
                None
            }
        };
        let discipline_selector = self.discipline_selector(accent, cx);
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
        // `self.dragging` is only ever *set*, never explicitly cleared (there's
        // no "drag ended" hook to clear it from — see `on_drag_move` below), so
        // it can go stale after a drag finishes. `cx.has_active_drag()` is the
        // real, always-accurate gate; combining both means the card only ever
        // reads as "the one being dragged" while a drag is genuinely ongoing.
        let is_dragging = self.dragging && cx.has_active_drag();
        // Fallback only matters for a drag started before the card's ever
        // painted once (not reachable in practice — you can't grab a handle
        // that hasn't rendered), so it's not worth chasing precision on.
        let ghost_width = self
            .card_bounds
            .get()
            .map(|b| b.size.width)
            .unwrap_or(px(420.));
        let (flip_offset, flip_generation) = self.flip.get();
        let motion = cx.theme().skin.motion(Duration::from_millis(220));

        let on_select = self.on_select.clone();
        let click_inputs = self.input_handles();
        let out_inputs = click_inputs.clone();

        // An outer, purely structural wrapper around the actual visual card:
        // the FLIP animation below applies a paint-time `top` offset to the
        // *inner* div, and that offset must not leak into what the bounds
        // probe measures — `top`/inset bakes into an element's own computed
        // bounds (matching CSS `position: relative` — an offset element's
        // `getBoundingClientRect()` moves too), so a probe *inside* the
        // animated div would see its own mid-flight slide as a stream of new
        // "jumps", constantly re-kicking the animation instead of settling.
        // The probe sits on this outer wrapper instead, which is never
        // offset, so it only ever reports genuine reorder-driven jumps.
        div()
            .id(("table-card-slot", editor_id))
            .relative()
            .child({
                let capture = self.card_bounds.clone();
                let flip = self.flip.clone();
                canvas(
                    move |bounds, window, cx| {
                        if let Some(old) = capture.get()
                            && cx.has_active_drag()
                        {
                            let dy = old.origin.y - bounds.origin.y;
                            if dy.abs() > px(1.) {
                                let (_, generation) = flip.get();
                                flip.set((dy, generation.wrapping_add(1)));
                            }
                        }
                        if capture.get() != Some(bounds) {
                            capture.set(Some(bounds));
                            window.request_animation_frame();
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full()
            })
            .child({
                // On the glass tier (not while this card is the drag
                // source — that placeholder stays a plain flat box, see
                // below) the card is real Liquid Glass: a `GlassRole::Card`
                // region clipped to the detail scroll pane, in place of the
                // resting-state gpui border + fill. Selection / conflict
                // state is a second, accent-tinted `GlassRole::CardOverlay`
                // region layered above it and inset a few points on every
                // side — not a tint on the card's own glass (merging a
                // strongly-tinted region with the overlapping neutral one
                // underneath blends into a flat, muddy wash instead of
                // reading as its own accent-coloured glass, see
                // `GlassRole::CardOverlay`'s doc comment) and not a ring
                // border either — the inset itself, leaving a sliver of the
                // plain card glass visible as a frame, is what reads as
                // "selected"/"conflicted" here.
                let native = glass::active();
                let viewport = self.viewport.get();
                let show_glass = native && !is_dragging && viewport.is_some();
                let overlay_tint = if conflicted {
                    let alpha = match theme.appearance {
                        Appearance::Light => 0.30,
                        Appearance::Dark => 0.38,
                    };
                    Some(gpui_kit::Hsla { a: alpha, ..c.warn })
                } else if selected {
                    let alpha = match theme.appearance {
                        Appearance::Light => 0.30,
                        Appearance::Dark => 0.38,
                    };
                    Some(gpui_kit::Hsla { a: alpha, ..c.primary })
                } else {
                    None
                };

                div()
                    .id(("table-card", editor_id))
                    .relative()
                    .flex()
                    .flex_col()
                    .rounded(theme.skin.radius_lg_px())
                    .when(show_glass, |el| {
                        el.child(glass::region_in_viewport(
                            glass_id(editor_id),
                            GlassRole::Card,
                            viewport.expect("checked by show_glass"),
                            glass::DETAIL_SCROLL_GROUP,
                            None,
                        ))
                    })
                    .when(!show_glass, |el| {
                        el.border_1()
                            .when(is_dragging, |el| el.border_dashed())
                            .border_color(if is_dragging {
                                c.line_strong
                            } else if conflicted {
                                c.warn
                            } else if selected {
                                c.primary
                            } else {
                                c.border
                            })
                            .bg(if is_dragging {
                                c.background
                            } else if selected {
                                c.accent_soft
                            } else {
                                c.surface
                            })
                    })
                    .when_some(overlay_tint.filter(|_| show_glass), |el, tint| {
                        el.child(
                            div().absolute().inset(px(6.)).child(glass::region_in_viewport(
                                overlay_glass_id(editor_id),
                                GlassRole::CardOverlay,
                                viewport.expect("checked by show_glass"),
                                glass::DETAIL_SCROLL_GROUP,
                                Some(tint),
                            )),
                        )
                    })
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
                    // Touch's right click: a long press.
                    .on_aux_click(cx.listener(|this, event: &gpui_kit::ClickEvent, _window, cx| {
                        if matches!(event, gpui_kit::ClickEvent::Touch(_)) && event.is_secondary() {
                            this.open_context_menu(event.position(), cx);
                        }
                    }))
                    // Tracks whether *this* card is the one currently being dragged —
                    // fires for every move of any active `DragTable` drag, regardless
                    // of where the pointer is (see `judging_table.rs`'s cursor-style
                    // fix for the same mechanism), so each card can tell itself apart
                    // from the one under the cursor purely from the payload it carries.
                    // Reordering itself (the live preview + the actual drop) is
                    // handled centrally by `DetailView::phase_body`, not here — see
                    // its doc comment for why per-card hover reporting didn't work.
                    .on_drag_move::<DragTable>(cx.listener(
                        |this, event: &DragMoveEvent<DragTable>, _window, cx| {
                            let dragging = event.drag(cx).editor_id == this.editor.entity_id();
                            if this.dragging != dragging {
                                this.dragging = dragging;
                                cx.notify();
                            }
                        },
                    ))
                    .child(
                        // The header + slot fields, together — hidden (not removed:
                        // `.invisible()` keeps its layout box, which is exactly the
                        // "empty rectangle, same footprint, as if already dragged
                        // away" the card needs to read as) while this card is the
                        // drag source, so its real judging table only ever appears
                        // once on screen: as the drag ghost following the cursor.
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(10.))
                            .p(px(12.))
                            .when(is_dragging, |el| el.invisible())
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap(px(8.))
                                    .child(
                                        div()
                                            .id("drag-handle")
                                            .flex_none()
                                            // Open hand while just hovering/about to
                                            // grab; `.cursor_move()` is `ClosedHand`
                                            // despite the name (a gpui naming trap) —
                                            // that showed the "grabbing" cursor before
                                            // any drag had actually started. The
                                            // drag's own cursor is a *separate*
                                            // mechanism: it's captured once, from this
                                            // same style, the instant the drag begins
                                            // (`AnyDrag::cursor_style`, read off this
                                            // div's style at that moment) and then
                                            // overrides every other cursor for the
                                            // rest of the gesture — so it would
                                            // otherwise stay `OpenHand` for the whole
                                            // drag too. `on_drag_move` flips it to
                                            // `ClosedHand` on the first move after the
                                            // drag starts (`cx.active_drag` only
                                            // exists by then; doing this from
                                            // `on_drag`'s own constructor below is
                                            // too early — it runs before gpui sets
                                            // `cx.active_drag`, so the setter would
                                            // no-op).
                                            .cursor_grab()
                                            .child(
                                                Icon::Grip.size(px(14.)).color(c.muted_foreground),
                                            )
                                            .on_drag(
                                                DragTable {
                                                    editor_id,
                                                    label: drag_label,
                                                    kind: table.kind.clone(),
                                                    width: ghost_width,
                                                },
                                                |dragged: &DragTable, _pos, _window, cx| {
                                                    cx.new(|_| dragged.clone())
                                                },
                                            )
                                            .on_drag_move::<DragTable>(|_, window, cx| {
                                                if cx.active_drag_cursor_style()
                                                    != Some(CursorStyle::ClosedHand)
                                                {
                                                    cx.set_active_drag_cursor_style(
                                                        CursorStyle::ClosedHand,
                                                        window,
                                                    );
                                                }
                                            }),
                                    )
                                    .child(
                                        div().flex_1().child(
                                            Field::new("table-label", &self.label_input)
                                                .accent(accent),
                                        ),
                                    )
                                    .child(discipline_selector)
                                    .child(
                                        Button::icon("del-table", Icon::Trash).small().on_click(
                                            cx.listener(|this, _, window, cx| {
                                                (this.on_delete.clone())(window, cx)
                                            }),
                                        ),
                                    ),
                            )
                            .child(div().flex().flex_wrap().gap(px(8.)).children(
                                self.slots.iter().map(|(label, input)| {
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap(px(3.))
                                        .w(px(148.))
                                        .child(
                                            div()
                                                .text_size(px(9.))
                                                .text_color(accent.unwrap_or(c.muted_foreground))
                                                .child(*label),
                                        )
                                        .child(
                                            Field::new(*label, input)
                                                .invalid(self.conflict_roles.contains(label))
                                                .accent(accent),
                                        )
                                }),
                            )),
                    )
                    .children(context_menu_layer)
                    // The FLIP reflow slide: animates `top` from `flip_offset`
                    // back to `0` (a pure paint-time offset — `.relative()`
                    // above, plus `top` being an inset rather than a margin,
                    // means it never affects sibling layout, only where
                    // *this* card paints) whenever the outer wrapper's probe
                    // detects the card jumped to a new index. The
                    // `flip_generation` in the id forces a fresh
                    // `with_animation` element-state (keyed by id) each time,
                    // rather than resuming whatever state a previous,
                    // unrelated tween left behind.
                    .with_animation(
                        SharedString::from(format!("table-flip-{editor_id:?}-{flip_generation}")),
                        Animation::new(motion).with_easing(ease_out_quint()),
                        move |el, delta| el.top(flip_offset * (1.0 - delta)),
                    )
            })
    }
}
