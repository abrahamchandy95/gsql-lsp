//! Semantic analysis of GSQL files.

mod builder;
mod model;

pub use builder::{analyze, analyze_in, canonical_accumulator, collapse_whitespace, type_of_type_node};
pub use model::*;
