//! Ranking and compression: what actually goes in the context window.

pub mod bm25;
pub mod density;
pub mod slimmer;

pub use bm25::Bm25;
pub use density::{Density, density};
pub use slimmer::{Context, Selected, SlimConfig, SourceRef, slim};
