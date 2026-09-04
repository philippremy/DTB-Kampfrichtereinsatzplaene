//! The platform seam.
//!
//! This module — and only this module — is allowed to contain
//! `#[cfg(target_os = …)]`. Everything else reads `cx.theme().skin` metrics or
//! goes through the helpers here. See the "cfg! boundary" section of the design
//! proposal.

pub mod decorations;
pub mod menu;
pub mod titlebar;
pub mod window;
