//! The application's own component layer.
//!
//! Built on `gpui-base` behaviour primitives + `gpui` elements; every component
//! reads colours from `cx.theme().color.*` and metrics from `cx.theme().skin.*`
//! — no literal `Hsla` or platform `cfg!`.

pub mod button;
pub mod checkbox;
pub mod chip;
pub mod context_menu;
pub mod field;
pub mod focus;
pub mod gallery;
pub mod glass_card;
pub mod icon;
pub mod kbd;
pub mod menu_bar;
pub mod org_emblem;
pub mod overlay_host;
pub mod popover;
pub mod segmented;
pub mod spinner;
pub mod template_tile;
pub mod toggle;
pub mod toolbar_group;
pub mod tooltip;

// Consumed by the shell / detail views from Phase 3 on.
#[allow(unused_imports)]
pub use button::{Button, ButtonSize, ButtonTone};
#[allow(unused_imports)]
pub use checkbox::Checkbox;
#[allow(unused_imports)]
pub use chip::{Chip, ChipTone};
#[allow(unused_imports)]
pub use field::Field;
#[allow(unused_imports)]
pub use icon::{Icon, IconElement};
#[allow(unused_imports)]
pub use kbd::Kbd;
#[allow(unused_imports)]
pub use menu_bar::MenuBar;
#[allow(unused_imports)]
pub use org_emblem::OrgEmblem;
#[allow(unused_imports)]
pub use overlay_host::OverlayHost;
#[allow(unused_imports)]
pub use popover::PopoverAnchor;
#[allow(unused_imports)]
pub use segmented::Segmented;
#[allow(unused_imports)]
pub use spinner::Spinner;
#[allow(unused_imports)]
pub use template_tile::TemplateTile;
#[allow(unused_imports)]
pub use toggle::Toggle;
