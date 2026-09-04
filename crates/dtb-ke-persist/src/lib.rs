//! Persistence layer: turso (SQLite-compatible) storage of postcard-encoded
//! [`dtb_ke_types::CompetitionDTO`] blobs.
//!
//! This crate is deliberately free of any UI / gpui dependency. Every query is
//! an `async fn` over a `turso::Connection`; the caller decides which executor
//! drives it.

pub mod database;
pub mod error;
pub mod serialization;

pub use database::*;
pub use error::*;
pub use serialization::*;
