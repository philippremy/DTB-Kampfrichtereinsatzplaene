//! A capsule that groups related toolbar controls.
//!
//! On the native glass tier (macOS 26+) the capsule is real Liquid Glass — a
//! [`glass`] region — and its own gpui fill stays transparent so the glass
//! shows through. Everywhere else it is a token-filled, bordered pill.
//!
//! Like interactive Liquid Glass, pressing anywhere in a group makes the
//! **whole group** swell slightly and settle back: the glass capsule grows
//! about its centre and the icons grow with it. The native glass views sit
//! under gpui and never see the mouse, so the bump is driven from here (a
//! press timestamp in element state, a scale computed per frame — see
//! [`Bump`]). The layout footprint never changes, so neighbours don't move.
//! It exists only on the glass tier: other platforms do no scaling at all.

use std::f32::consts::PI;
use std::time::{Duration, Instant};

use gpui::prelude::FluentBuilder;
use gpui::{
    AnyElement, InteractiveElement, IntoElement, MouseButton, ParentElement, RenderOnce, Styled,
    Window, div, px,
};

use crate::components::button::Button;
use crate::skin::glass::{self, GlassRole};
use crate::theme::ActiveTheme;

/// Capsule height. Buttons inside are 28 px, leaving 4 px of margin.
pub const GROUP_HEIGHT: f32 = 36.0;

/// How long a press bump lasts, before the skin's motion scale.
const BUMP_DURATION: Duration = Duration::from_millis(250);
/// How much larger the group gets at the peak (5 %).
const BUMP_PEAK: f32 = 0.05;

/// When the group was last pressed.
#[derive(Default)]
struct PressState {
    at: Option<Instant>,
}

enum Item {
    /// A button the group can scale with the bump.
    Button(Button),
    Other(AnyElement),
}

#[derive(IntoElement)]
pub struct ToolbarGroup {
    id: &'static str,
    prominent: bool,
    chromeless: bool,
    items: Vec<Item>,
}

impl ToolbarGroup {
    /// `id` must be unique per window (it keys the native glass view).
    pub fn new(id: &'static str) -> Self {
        Self {
            id,
            prominent: false,
            chromeless: false,
            items: Vec::new(),
        }
    }

    /// The accent-tinted capsule for the toolbar's primary action.
    pub fn prominent(mut self) -> Self {
        self.prominent = true;
        self
    }

    /// A single standalone button (not a *group* of related actions, which
    /// still wants a capsule to visually tie them together) — on the macOS
    /// fallback tier ([`glass::mac_fallback`]) this draws no capsule chrome
    /// at rest at all, relying entirely on the button's own `Ghost`-tone
    /// hover for feedback, matching a plain unified-toolbar button. No
    /// effect on the glass tier (still real glass) or on Windows/Linux
    /// (unaffected — they have their own, deliberately different, non-glass
    /// look).
    pub fn chromeless(mut self) -> Self {
        self.chromeless = true;
        self
    }

    /// Adds a button that swells with the group's press bump. Prefer this to
    /// [`ParentElement::child`] for buttons.
    pub fn button(mut self, button: Button) -> Self {
        self.items.push(Item::Button(button));
        self
    }
}

impl ParentElement for ToolbarGroup {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.items.extend(elements.into_iter().map(Item::Other));
    }
}

/// The press-bump animation of one glass capsule (see the module docs).
///
/// Create it in `render` with a per-window-unique `id`; use [`Self::scale`]
/// for the glass region (and any content that swells with it) and attach
/// [`Self::on_press`] to the capsule's box. On any tier without native glass
/// the scale is always `1.0` and pressing does nothing.
pub struct Bump {
    state: gpui::Entity<PressState>,
    native: bool,
    /// The current scale factor: `1.0` at rest.
    pub scale: f32,
}

impl Bump {
    pub fn new(id: &'static str, window: &mut Window, cx: &mut gpui::App) -> Self {
        let state = window.use_keyed_state(id, cx, |_, _| PressState::default());
        let native = glass::active();
        let scale = match state.read(cx).at {
            Some(at) if native && !cx.reduce_motion() => {
                let total = cx.theme().skin.motion(BUMP_DURATION);
                let elapsed = at.elapsed();
                if elapsed < total {
                    window.request_animation_frame();
                }
                bump_scale(elapsed, total)
            }
            _ => 1.0,
        };
        Self {
            state,
            native,
            scale,
        }
    }

    /// A mouse-down listener that starts the bump.
    pub fn on_press(&self) -> impl Fn(&gpui::MouseDownEvent, &mut Window, &mut gpui::App) + 'static {
        let state = self.state.clone();
        let native = self.native;
        move |_, _, cx| {
            if native {
                state.update(cx, |state, cx| {
                    state.at = Some(Instant::now());
                    cx.notify();
                });
            }
        }
    }
}

/// The group's scale `elapsed` after a press: up quickly, back down slower.
fn bump_scale(elapsed: Duration, total: Duration) -> f32 {
    if total.is_zero() || elapsed >= total {
        return 1.0;
    }
    let x = elapsed.as_secs_f32() / total.as_secs_f32();
    // `x^0.6` moves the peak to ~⅓ of the way through.
    1.0 + BUMP_PEAK * (PI * x.powf(0.6)).sin()
}

impl RenderOnce for ToolbarGroup {
    fn render(self, window: &mut Window, cx: &mut gpui::App) -> impl IntoElement {
        let bump = Bump::new(self.id, window, cx);
        let theme = cx.theme();
        let c = &theme.color;
        let native = glass::active();
        let role = if self.prominent {
            GlassRole::CapsuleProminent
        } else {
            GlassRole::Capsule
        };
        let scale = bump.scale;

        // The glass probe must sit on an *unpadded* box (an absolute child
        // covers its parent's content box), so the padding lives on an inner
        // row and the capsule's own box carries the region + fill.
        div()
            .relative()
            .flex_none()
            .h(px(GROUP_HEIGHT))
            .rounded_full()
            .on_mouse_down(MouseButton::Left, bump.on_press())
            .when(native, |el| {
                el.child(glass::region_scaled(self.id, role, scale))
            })
            .when(!native, |el| {
                if self.chromeless && glass::mac_fallback() {
                    el
                } else {
                    let (fill, border) = if self.prominent {
                        (c.primary, c.primary)
                    } else {
                        (c.surface, c.border)
                    };
                    el.bg(fill).border_1().border_color(border).shadow_xs()
                }
            })
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(2.))
                    .h_full()
                    .px(px(4.))
                    .children(self.items.into_iter().map(|item| match item {
                        Item::Button(button) => button.scale(scale).into_any_element(),
                        Item::Other(element) => element,
                    })),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bump_starts_and_ends_at_rest() {
        let total = Duration::from_millis(250);
        assert_eq!(bump_scale(Duration::ZERO, total), 1.0);
        assert_eq!(bump_scale(total, total), 1.0);
        assert_eq!(bump_scale(total * 2, total), 1.0);
        // Zero-length (motion scale 0) never divides by zero.
        assert_eq!(bump_scale(Duration::ZERO, Duration::ZERO), 1.0);
    }

    #[test]
    fn bump_peaks_early_and_never_exceeds_the_peak() {
        let total = Duration::from_millis(250);
        let samples: Vec<f32> = (0..=32)
            .map(|i| bump_scale(total * i / 32, total))
            .collect();
        let (peak_at, peak) = samples
            .iter()
            .copied()
            .enumerate()
            .fold((0, 1.0f32), |best, (i, v)| if v > best.1 { (i, v) } else { best });
        assert!(peak > 1.0 + BUMP_PEAK * 0.9 && peak <= 1.0 + BUMP_PEAK + 1e-4, "peak {peak}");
        // Up faster than down: the peak is in the first half.
        assert!(peak_at < 16, "peak at sample {peak_at}");
        assert!(samples.iter().all(|s| *s >= 1.0));
    }
}
