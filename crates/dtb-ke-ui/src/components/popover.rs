//! Shared plumbing for the app's trigger-anchored popovers (the discipline
//! picker, org/date pickers, the language picker, the remarks colour
//! picker): capturing a trigger's absolute bounds via a probe canvas, then
//! floating content anchored just below it, above every dialog.
//!
//! Every caller still owns its own trigger element, popover content, and
//! dismiss handling (where `on_mouse_down_out` attaches differs on purpose —
//! see `detail/remarks.rs::color_button`'s doc comment for why it sits on
//! the panel rather than the trigger). `PopoverAnchor` only factors out the
//! bounds-capture/positioning incantation that was previously duplicated
//! byte-for-byte across five call sites.

use std::cell::Cell;
use std::rc::Rc;

use gpui_kit::{
    AnyElement, Bounds, IntoElement, ParentElement, Pixels, Styled, anchored, canvas, deferred,
    point,
};

/// A trigger's captured absolute bounds, shared between the probe canvas
/// (painted once per frame) and the popover's own positioning (read when
/// open) without an entity round-trip.
#[derive(Clone)]
pub struct PopoverAnchor(Rc<Cell<Option<Bounds<Pixels>>>>);

impl PopoverAnchor {
    pub fn new() -> Self {
        Self(Rc::new(Cell::new(None)))
    }

    pub fn get(&self) -> Option<Bounds<Pixels>> {
        self.0.get()
    }

    /// An invisible probe that records the *content* box of whatever it's
    /// placed inside — put it as the trigger wrapper's first child, on a
    /// wrapper with no padding/border of its own, so the popover aligns with
    /// the trigger's real visible edge (a probe placed inside a padded
    /// trigger would offset the popover by that padding).
    pub fn probe(&self) -> AnyElement {
        let capture = self.0.clone();
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
        .size_full()
        .into_any_element()
    }

    /// Float `content` anchored `gap` below the captured trigger bounds,
    /// above every dialog (`gpui_kit::base::POPUP_PRIORITY` — a
    /// `gpui_kit::base::Dialog` paints at priority `10 + layer`, so anything
    /// meant to float above one needs this). `None` until the probe has
    /// captured a bounds at least once (the first frame or two after open).
    pub fn float_below(&self, gap: Pixels, content: impl IntoElement) -> Option<AnyElement> {
        let b = self.get()?;
        let anchor = point(b.origin.x, b.origin.y + b.size.height + gap);
        Some(
            deferred(anchored().position(anchor).snap_to_window().child(content))
                .with_priority(gpui_kit::base::POPUP_PRIORITY)
                .into_any_element(),
        )
    }
}

impl Default for PopoverAnchor {
    fn default() -> Self {
        Self::new()
    }
}
