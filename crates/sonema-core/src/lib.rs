//! Stable domain model and non-destructive editing rules for Sonema.
//!
//! This crate intentionally knows nothing about CPAL, egui, files, or operating
//! systems. Audio backends and user interfaces can be replaced without changing
//! project semantics.

mod edit;
mod history;
mod model;

pub use edit::{EditError, SnapMode};
pub use history::History;
pub use model::*;
