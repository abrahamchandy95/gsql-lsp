//! A language server for TigerGraph GSQL, built on the tree-sitter-gsql grammar.

pub mod analysis;
pub mod builtin_docs;
pub mod builtins;
pub mod check;
pub mod document;
pub mod editor_config;
pub mod features;
pub mod format;
pub mod lsp;
pub mod server;
pub mod syntax;
pub mod text;
pub mod uri;
pub mod workspace;

/// Stack size for threads that analyze GSQL. Deeply nested input (e.g.
/// generated expressions with tens of thousands of terms) produces deep syntax
/// trees that the analysis walks recursively; a large stack keeps that from
/// aborting the process. Only the pages actually used are committed.
pub const STACK_SIZE: usize = 256 * 1024 * 1024;
