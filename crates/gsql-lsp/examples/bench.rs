//! Times each analysis phase on a file:
//! `cargo run --release --example bench -- path/to/file.gsql`

use std::time::Instant;

use gsql_lsp::features::{self, Config, Snapshot};
use gsql_lsp::text::{PositionEncoding, SourceText};
use gsql_lsp::workspace::{FileIndex, Workspace};
use gsql_lsp::{analysis, syntax};

fn main() {
    // Same large stack as the server, for deeply nested input.
    std::thread::Builder::new().stack_size(gsql_lsp::STACK_SIZE).spawn(run).unwrap().join().unwrap();
}

fn run() {
    let path = std::env::args().nth(1).expect("usage: bench <file.gsql>");
    let text = std::fs::read_to_string(path).expect("readable file");
    let mut parser = syntax::new_parser();

    let start = Instant::now();
    let tree = syntax::parse(&mut parser, &text, None);
    println!("parse        {:?}", start.elapsed());

    let start = Instant::now();
    let analysis = analysis::analyze(&tree, &text);
    println!(
        "analyze      {:?} ({} references, {} scopes)",
        start.elapsed(),
        analysis.references.len(),
        analysis.scopes.len()
    );

    let source = SourceText::new(text.clone());
    let uri = "file:///bench.gsql";
    let start = Instant::now();
    let index = FileIndex::build(uri, &analysis, &source, PositionEncoding::Utf16);
    println!("index        {:?}", start.elapsed());

    let mut workspace = Workspace::default();
    workspace.update(index);
    let config = Config::default();
    let snapshot = Snapshot {
        uri,
        source: &source,
        tree: &tree,
        analysis: &analysis,
        workspace: &workspace,
        encoding: PositionEncoding::Utf16,
        config: &config,
    };
    let start = Instant::now();
    let diagnostics = features::diagnostics::diagnostics(&snapshot);
    println!("diagnostics  {:?} ({})", start.elapsed(), diagnostics.len());
    let start = Instant::now();
    let tokens = features::semantic_tokens::semantic_tokens(&snapshot, None);
    println!("tokens       {:?} ({})", start.elapsed(), tokens.len() / 5);
    let start = Instant::now();
    let folds = features::folding::folding_ranges(&snapshot);
    println!("folding      {:?} ({})", start.elapsed(), folds.len());
    let start = Instant::now();
    let symbols = features::symbols::document_symbols(&snapshot);
    println!("symbols      {:?} ({})", start.elapsed(), symbols.len());
}
