//! Editor-side ("frontend") data model.
//!
//! These types are deliberately separate from the `…DTO` types in
//! `dtb-ke-types`: the DTOs define the on-disk wire format and should only
//! change deliberately, whereas these types are shaped for convenient in-place
//! editing and can grow UI-only state later (selection, validation caches, …)
//! without touching what gets serialized.
//!
//! Only the *aggregate* shape differs. Leaf value types (`OrganizationDTO`,
//! `MeetingTimeDTO`, `JudgingTableKindDTO`, …) are reused from `dtb-ke-types`
//! as-is — duplicating those buys nothing.
//!
//! Conversions live next to the types: `From<CompetitionDTO> for Competition`
//! (decode, consumes the DTO) and `From<&Competition> for CompetitionDTO`
//! (encode, borrows the model).

mod competition;
mod conflicts;
mod rich_text;
pub mod roles;

// Some of these are only consumed by the not-yet-written UI layer.
#[allow(unused_imports)]
pub use competition::{Competition, CompetitionMeta, JudgingTable, Round};
#[allow(unused_imports)]
pub use conflicts::{Conflict, PhaseConflicts, Placement, detect_phase};
#[allow(unused_imports)]
pub use rich_text::{Paragraph, RichText, Run};
