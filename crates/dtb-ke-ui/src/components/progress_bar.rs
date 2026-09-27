//! A progress bar: a track with a fill for a known fraction, or a sliding segment when the total is unknown.
//!
//! The fraction is the caller's to compute (`done / total`); `None` means "working, cannot say how far" — the
//! indeterminate segment slides through `gpui_kit::with_animation`, which snaps it to a static position when
//! `App::reduce_motion` is set. Prefer a real fraction wherever one exists.

use std::time::Duration;

use gpui_kit::{
    Animation, AnimationExt, App, IntoElement, ParentElement, Pixels, RenderOnce, Styled, Window,
    div, px, relative,
};

use crate::theme::ActiveTheme;

#[derive(IntoElement)]
pub struct ProgressBar {
    fraction: Option<f32>,
    height: Pixels,
}

impl ProgressBar {
    /// `fraction` in `0.0..=1.0`, or `None` for an indeterminate bar.
    pub fn new(fraction: Option<f32>) -> Self {
        Self {
            fraction,
            height: px(6.),
        }
    }

    pub fn height(mut self, height: impl Into<Pixels>) -> Self {
        self.height = height.into();
        self
    }
}

impl RenderOnce for ProgressBar {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let c = cx.theme().color;
        let track = div()
            .relative()
            .w_full()
            .h(self.height)
            .rounded_full()
            .bg(c.border)
            .overflow_hidden();
        match self.fraction {
            Some(fraction) => track.child(
                div()
                    .h_full()
                    .w(relative(fraction.clamp(0.0, 1.0)))
                    .rounded_full()
                    .bg(c.primary),
            ),
            None => track.child(
                div()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(relative(0.3))
                    .rounded_full()
                    .bg(c.primary)
                    .with_animation(
                        "indeterminate",
                        Animation::new(Duration::from_millis(1300)).repeat(),
                        |segment, t| segment.left(relative(t * 0.7)),
                    ),
            ),
        }
    }
}
