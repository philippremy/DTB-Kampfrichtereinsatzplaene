//! The application icon set.
//!
//! Icons are single-colour SVGs compiled into the binary and rendered as a
//! tinted mask by gpui (`svg().data(..)` — the SVG's own colours are ignored,
//! the current `text_color` fills the shape). Default size 16px, default colour
//! `theme.color.foreground`.

use gpui::{App, Hsla, IntoElement, Pixels, RenderOnce, Styled, Window, px, svg};

use crate::theme::ActiveTheme;

macro_rules! icons {
    ($($variant:ident => $file:literal),+ $(,)?) => {
        /// A named icon.
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum Icon {
            $($variant),+
        }

        impl Icon {
            fn svg(self) -> &'static str {
                match self {
                    $(Self::$variant => include_str!(concat!("../../assets/icons/", $file))),+
                }
            }

            /// All icons, for the gallery.
            pub const ALL: &'static [Icon] = &[$(Self::$variant),+];

            /// A short label, for the gallery.
            pub fn label(self) -> &'static str {
                match self {
                    $(Self::$variant => stringify!($variant)),+
                }
            }
        }
    };
}

icons! {
    Search      => "search.svg",
    Close       => "close.svg",
    ChevronDown => "chevron-down.svg",
    Plus        => "plus.svg",
    Check       => "check.svg",
    Warning     => "warning.svg",
    AlertCircle => "alert-circle.svg",
    PanelLeft   => "panel-left.svg",
    Settings    => "settings.svg",
    Copy        => "copy.svg",
    Trash       => "trash.svg",
    Sun         => "sun.svg",
    Moon        => "moon.svg",
    Monitor     => "monitor.svg",
    Menu        => "menu.svg",
    Grip        => "grip.svg",
    WindowMinimize => "window-minimize.svg",
    WindowMaximize => "window-maximize.svg",
    WindowRestore  => "window-restore.svg",
    Preview => "preview.svg",
    Export => "export.svg",
    Package => "package.svg",
    Download => "download.svg",
    ExternalLink => "external-link.svg",
    RotateCw => "rotate-cw.svg",
    Document => "document.svg",
    Calendar => "calendar.svg"
}

impl Icon {
    /// Render at a given size, current theme foreground colour.
    pub fn size(self, size: impl Into<Pixels>) -> IconElement {
        IconElement {
            icon: self,
            size: size.into(),
            color: None,
        }
    }

    /// Render at 16px, current theme foreground colour.
    pub fn small(self) -> IconElement {
        self.size(px(16.))
    }
}

/// A sized, coloured icon element.
#[derive(IntoElement)]
pub struct IconElement {
    icon: Icon,
    size: Pixels,
    color: Option<Hsla>,
}

impl IconElement {
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl From<Icon> for IconElement {
    fn from(icon: Icon) -> Self {
        icon.size(px(16.))
    }
}

impl RenderOnce for IconElement {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().color.foreground);
        svg()
            .flex_none()
            .size(self.size)
            .text_color(color)
            .data(self.icon.svg().as_bytes())
    }
}
