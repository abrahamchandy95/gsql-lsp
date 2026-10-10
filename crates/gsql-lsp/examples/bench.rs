//! Times each analysis phase on a file:
//! `cargo run --release --example bench -- path/to/file.gsql`

use std::time::{Duration, Instant};

use gsql_lsp::features::{self, Config, Snapshot};
use gsql_lsp::text::{PositionEncoding, SourceText};
use gsql_lsp::workspace::{FileIndex, Workspace};
use gsql_lsp::{analysis, syntax};

fn main() {
    // allocate a large stack on the server
    std::thread::Builder::new()
        .stack_size(gsql_lsp::STACK_SIZE)
        .spawn(run)
        .unwrap()
        .join()
        .unwrap();
}

fn timed<T>(f: impl FnOnce() -> T) -> (T, Duration) {
    let start = Instant::now();
    let res = f();
    (res, start.elapsed())
}

fn run() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: bench <file.gsql>");
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read '{path}': {e}"));
    println!("Benchmarking: {path} ({} bytes)", text.len());
    println!("{:-<40}", "");

    let started = Instant::now();
    let mut parser = syntax::new_parser();

    let (tree, d) = timed(|| syntax::parse(&mut parser, &text, None));
    println!("{:<12} {:?}", "parse", d);

    let (analysis, d) =
        timed(|| analysis::Analysis::from_tree(&tree, &text, None));
    println!(
        "{:<12} {:?} ({} refs, {} scopes)",
        "analyze",
        d,
        analysis.references.len(),
        analysis.scopes.len()
    );

    let source = SourceText::new(text);
    let uri = "file:///bench.gsql";

    let (index, d) = timed(|| {
        FileIndex::build(uri, &analysis, &source, PositionEncoding::Utf16)
    });
    println!("{:<12} {:?}", "index", d);

    let mut workspace = Workspace::default();
    workspace.update(index);
    let config = Config::default();
    let current_state = Snapshot {
        uri,
        source: &source,
        tree: &tree,
        analysis: &analysis,
        workspace: &workspace,
        encoding: PositionEncoding::Utf16,
        config: &config,
    };

    let (diagnostics, d) =
        timed(|| features::diagnostics::diagnostics(&current_state));
    println!(
        "{:<12} {:?} ({} items)",
        "diagnostics",
        d,
        diagnostics.len()
    );

    let (tokens, d) = timed(|| {
        features::semantic_tokens::semantic_tokens(&current_state, None)
    });
    println!("{:<12} {:?} ({} tokens)", "tokens", d, tokens.len() / 5);

    let (folds, d) =
        timed(|| features::folding::folding_ranges(&current_state));
    println!("{:<12} {:?} ({} ranges)", "folding", d, folds.len());

    let (symbols, d) =
        timed(|| features::symbols::document_symbols(&current_state));
    println!("{:<12} {:?} ({} symbols)", "symbols", d, symbols.len());
    println!("{:-<40}", "");
    println!("{:<12} {:?}", "total", started.elapsed());
}
