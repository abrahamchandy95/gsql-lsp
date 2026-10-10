//! Randomized robustness test. Random edits are applied to GSQL sources through
//! the incremental document path, then every language feature runs on the
//! result. The test fails if a feature panics, if the edited text differs from
//! an independent model of the edits, or if incremental reparsing diverges
//! from a fresh parse.
//!
//! More iterations:  GSQL_STRESS_ITERATIONS=20000 cargo test --release --test stress
//! Another seed:     GSQL_STRESS_SEED=42 cargo test --test stress

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use gsql_lsp::analysis;
use gsql_lsp::document::Document;
use gsql_lsp::features::{self, Config, Snapshot};
use gsql_lsp::lsp::types::{
    FormattingOptions, Position, Range, TextDocumentContentChangeEvent,
};
use gsql_lsp::syntax;
use gsql_lsp::text::{PositionEncoding, SourceText};
use gsql_lsp::workspace::{FileIndex, Workspace};

/// xorshift64*: small, deterministic and good enough for test inputs.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }

    fn chance(&mut self, percent: u64) -> bool {
        self.next() % 100 < percent
    }
}

const FRAGMENTS: &[&str] = &[
    "",
    " ",
    "\n",
    "\n\n",
    "\t",
    ";",
    ",",
    "(",
    ")",
    "{",
    "}",
    "[",
    "]",
    "<",
    ">",
    ".",
    ":",
    "@",
    "@@",
    "\"",
    "'",
    "$",
    "#",
    "//",
    "/*",
    "*/",
    "-",
    "->",
    "<-",
    "-(",
    ")-",
    "=",
    "+=",
    "==",
    "*",
    "%",
    "..",
    "|",
    "SELECT",
    "FROM",
    "WHERE",
    "ACCUM",
    "POST-ACCUM",
    "IF",
    "THEN",
    "ELSE",
    "ELSE IF",
    "END",
    "WHILE",
    "DO",
    "FOREACH",
    "IN",
    "CASE",
    "WHEN",
    "RETURN",
    "PRINT",
    "INSERT INTO",
    "VALUES",
    "INT",
    "STRING",
    "VERTEX<Person>",
    "SumAccum<INT>",
    "MapAccum<STRING, SumAccum<INT>>",
    "@@total",
    "@score",
    "s.@score'",
    "t.",
    "Person",
    "Knows",
    "Person:s",
    "-(Knows:e)-",
    "-(Knows>*1..3)-",
    "(s:Person)-[e:Knows]->(t)",
    "{Person.*}",
    "abs(",
    "count(",
    "\"str\\\"ing\"",
    "é",
    "世界",
    "🎉",
    "𝄞",
    "CREATE QUERY q(INT k) {",
    "INSTALL QUERY q",
    "RUN QUERY q(1, \"x\")",
    "LOAD f TO VERTEX Person VALUES ($0, $\"name\");",
    "CREATE VERTEX V (PRIMARY_ID id STRING)",
    "TYPEDEF TUPLE <INT a> T;",
    "INTERVAL 1 DAY",
    "RANGE[0, 9]",
    "OPENCYPHER",
    "MATCH (n) RETURN n",
];

/// Width of a character in the given position encoding.
fn units(c: char, encoding: PositionEncoding) -> usize {
    match encoding {
        PositionEncoding::Utf8 => c.len_utf8(),
        PositionEncoding::Utf16 => c.len_utf16(),
        PositionEncoding::Utf32 => 1,
    }
}

/// The text model: a naive, independent implementation of LSP positions.
fn model_offset(
    text: &str,
    position: Position,
    encoding: PositionEncoding,
) -> usize {
    let mut line = 0;
    let mut offset = 0;
    for (index, c) in text.char_indices() {
        if line == position.line as usize {
            break;
        }
        if c == '\n' {
            line += 1;
        }
        offset = index + c.len_utf8();
    }
    if line < position.line as usize {
        return text.len();
    }
    let mut used = 0;
    for (index, c) in text[offset..].char_indices() {
        if c == '\n'
            || c == '\r' && text[offset + index..].starts_with("\r\n")
        {
            return offset + index;
        }
        let width = units(c, encoding);
        if used + width > position.character as usize {
            return offset + index;
        }
        used += width;
        if used == position.character as usize {
            return offset + index + c.len_utf8();
        }
    }
    text.len()
}

fn random_position(
    rng: &mut Rng,
    text: &str,
    encoding: PositionEncoding,
) -> Position {
    let lines = text.split('\n').count() as u32;
    let line = rng.below(lines as usize + 1) as u32;
    let width: usize = text
        .split('\n')
        .nth(line as usize)
        .map(|l| l.chars().map(|c| units(c, encoding)).sum())
        .unwrap_or(0);
    Position::new(line, rng.below(width + 3) as u32)
}

fn random_insert(rng: &mut Rng, sources: &[String]) -> String {
    if rng.chance(15) {
        // A chunk of real GSQL.
        let source = &sources[rng.below(sources.len())];
        let chars: Vec<char> = source.chars().collect();
        let start = rng.below(chars.len());
        let len = rng.below(120.min(chars.len() - start) + 1);
        return chars[start..start + len].iter().collect();
    }
    let mut text = String::new();
    for _ in 0..=rng.below(3) {
        text.push_str(FRAGMENTS[rng.below(FRAGMENTS.len())]);
    }
    text
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gsql_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        let path = entry.path();
        if path.is_dir() {
            gsql_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "gsql") {
            out.push(path);
        }
    }
}

/// Runs `f`, turning a panic into a failure message that includes the input.
fn guard<T>(name: &str, text: &str, f: impl FnOnce() -> T) -> T {
    let started = std::time::Instant::now();
    let result = guard_inner(name, text, f);
    if std::env::var("GSQL_STRESS_TIMING").is_ok()
        && started.elapsed().as_millis() > 100
    {
        eprintln!("{name}: {:?} ({} bytes)", started.elapsed(), text.len());
    }
    result
}

fn guard_inner<T>(name: &str, text: &str, f: impl FnOnce() -> T) -> T {
    match catch_unwind(AssertUnwindSafe(f)) {
        Ok(value) => value,
        Err(panic) => {
            let message = gsql_lsp::panic_message(panic);
            let dump = std::env::temp_dir().join("gsql-stress-failure.gsql");
            std::fs::write(&dump, text).unwrap();
            panic!(
                "{name} panicked: {message}\ninput written to {}",
                dump.display()
            );
        }
    }
}

fn exercise(
    text: &str,
    tree: &tree_sitter::Tree,
    workspace_files: &[FileIndex],
    encoding: PositionEncoding,
    rng: &mut Rng,
) {
    let uri = "file:///stress/main.gsql";
    let analysis = guard("analysis", text, || {
        analysis::Analysis::from_tree(tree, text, None)
    });
    let source = SourceText::new(text.to_string());
    let mut workspace = Workspace::default();
    for index in workspace_files {
        workspace.update(index.clone());
    }
    workspace.update(FileIndex::build(uri, &analysis, &source, encoding));
    let config = Config {
        semantic_tokens_lexical: rng.chance(50),
        format_keyword_case: [
            features::KeywordCase::Preserve,
            features::KeywordCase::Upper,
            features::KeywordCase::Lower,
        ][rng.below(3)],
        ..Config::default()
    };
    let snapshot = Snapshot {
        uri,
        source: &source,
        tree,
        analysis: &analysis,
        workspace: &workspace,
        encoding,
        config: &config,
    };
    let end = snapshot.position(text.len());
    let whole = Range::new(Position::new(0, 0), end);
    let diagnostics = guard("diagnostics", text, || {
        features::diagnostics::diagnostics(&snapshot)
    });
    guard("semantic tokens", text, || {
        features::semantic_tokens::semantic_tokens(&snapshot, None)
    });
    guard("folding", text, || {
        features::folding::folding_ranges(&snapshot)
    });
    guard("document symbols", text, || {
        features::symbols::document_symbols(&snapshot)
    });
    guard("workspace symbols", text, || {
        features::symbols::workspace_symbols(&workspace, "e")
    });
    guard("inlay hints", text, || {
        features::inlay_hints::inlay_hints(&snapshot, whole)
    });
    let options = FormattingOptions {
        insert_spaces: rng.chance(80),
        ..FormattingOptions::default()
    };
    guard("formatting", text, || {
        features::formatting::format(&snapshot, &options, None)
    });
    for _ in 0..3 {
        let position = random_position(rng, text, encoding);
        let range = Range::new(
            position,
            random_position(rng, text, encoding).max(position),
        );
        guard("range semantic tokens", text, || {
            features::semantic_tokens::semantic_tokens(&snapshot, Some(range))
        });
        guard("range formatting", text, || {
            features::formatting::format(&snapshot, &options, Some(range))
        });
        guard("hover", text, || {
            features::hover::hover(&snapshot, position)
        });
        guard("completion", text, || {
            features::completion::completion(
                &snapshot,
                position,
                rng.chance(50),
            )
        });
        guard("signature help", text, || {
            features::signature_help::signature_help(&snapshot, position)
        });
        guard("definition", text, || {
            features::navigation::definition(&snapshot, position)
        });
        guard("type definition", text, || {
            features::navigation::type_definition(&snapshot, position)
        });
        guard("references", text, || {
            features::navigation::references(&snapshot, position, true)
        });
        guard("highlights", text, || {
            features::navigation::document_highlight(&snapshot, position)
        });
        guard("prepare rename", text, || {
            features::navigation::prepare_rename(&snapshot, position).ok()
        });
        guard("rename", text, || {
            features::navigation::rename(&snapshot, position, "renamed").ok()
        });
        guard("selection ranges", text, || {
            features::selection::selection_ranges(&snapshot, &[position])
        });
        guard("code actions", text, || {
            features::code_actions::code_actions(
                &snapshot,
                range,
                &diagnostics,
                None,
                &diagnostics,
            )
        });
    }
}

#[test]
fn random_edits_never_break_the_server() {
    let iterations: usize = std::env::var("GSQL_STRESS_ITERATIONS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(300);
    let seed: u64 = std::env::var("GSQL_STRESS_SEED")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(0x5eed_6501);

    let mut paths = Vec::new();
    gsql_files(&repo().join("examples"), &mut paths);
    gsql_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        &mut paths,
    );
    paths.sort();
    let sources: Vec<String> = paths
        .iter()
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    // Schema files give cross-file lookups something to find.
    let workspace_files: Vec<FileIndex> = paths
        .iter()
        .zip(&sources)
        .filter(|(path, _)| path.to_string_lossy().contains("schema"))
        .map(|(path, text)| {
            let (_, analysis) = analysis::Analysis::parse(text);
            let uri = gsql_lsp::uri::from_path(path);
            FileIndex::build(
                &uri,
                &analysis,
                &SourceText::new(text.clone()),
                PositionEncoding::Utf16,
            )
        })
        .collect();

    let mut rng = Rng(seed);
    let mut parser = syntax::new_parser();
    let mut fresh_parser = syntax::new_parser();
    for iteration in 0..iterations {
        let encoding = [
            PositionEncoding::Utf8,
            PositionEncoding::Utf16,
            PositionEncoding::Utf32,
        ][rng.below(3)];
        let start = sources[rng.below(sources.len())].clone();
        let mut document = Document::new(
            "file:///stress/main.gsql".into(),
            0,
            start.clone(),
            &mut parser,
        );
        let mut model = start;
        for _ in 0..=rng.below(6) {
            let from = random_position(&mut rng, &model, encoding);
            let to = if rng.chance(40) {
                from
            } else {
                random_position(&mut rng, &model, encoding).max(from)
            };
            let insert = random_insert(&mut rng, &sources);
            let start_offset = model_offset(&model, from, encoding);
            let end_offset =
                model_offset(&model, to, encoding).max(start_offset);
            model.replace_range(start_offset..end_offset, &insert);
            let change = TextDocumentContentChangeEvent {
                range: Some(Range::new(from, to)),
                text: insert,
            };
            let before = document.text().to_string();
            guard("apply_changes", &before, || {
                document.apply_changes(
                    &[change],
                    None,
                    encoding,
                    &mut parser,
                );
            });
        }
        assert_eq!(
            document.text(),
            model,
            "edit application diverged from the model (iteration {iteration})"
        );
        let fresh = syntax::parse(&mut fresh_parser, &model, None);
        if !fresh.root_node().has_error() {
            assert_eq!(
                document.tree.root_node().to_sexp(),
                fresh.root_node().to_sexp(),
                "incremental parse diverged from a fresh parse (iteration {iteration})"
            );
        }
        exercise(
            &model,
            &document.tree,
            &workspace_files,
            encoding,
            &mut rng,
        );
    }
}

/// Machine-made code can nest far deeper than people write; no feature may
/// overflow the stack on it (the server runs with `STACK_SIZE`).
#[test]
fn deep_nesting_never_breaks_the_server() {
    let inputs = [
        format!(
            "CREATE QUERY q() {{\n  INT x = {}1{};\n}}\n",
            "(".repeat(100_000),
            ")".repeat(100_000)
        ),
        format!(
            "CREATE QUERY q() {{\n  {}INT{} @@x;\n}}\n",
            "ListAccum<".repeat(20_000),
            ">".repeat(20_000)
        ),
        format!(
            "CREATE QUERY q(BOOL b) {{\n{}PRINT 1;\n{}}}\n",
            "IF b THEN\n".repeat(5_000),
            "END;\n".repeat(5_000)
        ),
        format!(
            "CREATE QUERY q() {{\n  INT x = {};\n}}\n",
            vec!["abs(1)"; 20_000].join(" + ")
        ),
        format!(
            "CREATE QUERY q(INT y) {{\n  INT x = {};\n  PRINT {};\n}}\n",
            vec!["y"; 20_000].join(" + "),
            vec!["\"s\""; 20_000].join(" + ")
        ),
        format!(
            "CREATE QUERY q() {{\n  R = SELECT s FROM P:s ACCUM {}\n  PRINT R;\n}}\n",
            "IF TRUE THEN @@n += 1 ELSE ".repeat(2_000)
                + "@@n += 2"
                + &" END".repeat(2_000)
                + ";"
        ),
    ];
    std::thread::Builder::new()
        .stack_size(gsql_lsp::STACK_SIZE)
        .spawn(move || {
            let mut rng = Rng(7);
            for text in &inputs {
                let tree =
                    syntax::parse(&mut syntax::new_parser(), text, None);
                exercise(text, &tree, &[], PositionEncoding::Utf16, &mut rng);
            }
        })
        .unwrap()
        .join()
        .unwrap();
}
