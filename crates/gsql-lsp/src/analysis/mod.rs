//! Semantic analysis of GSQL files.

mod builder;
mod model;

pub use builder::{
    canonical_accumulator, collapse_whitespace, literal_type,
    type_of_type_node,
};
pub use model::*;
