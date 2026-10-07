//! docs/diagnostics.md has exactly one row for every diagnostic code the sources emit.

use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn is_code(text: &str) -> bool {
    text.len() >= 4
        && text.starts_with(|c: char| c.is_ascii_lowercase())
        && text.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// The text up to `len` bytes after `at`, cut at a character boundary.
fn window(text: &str, at: usize, len: usize) -> &str {
    let mut end = (at + len).min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[at..end]
}

/// The string literals of `text`, each with the text before it.
fn literals(text: &str) -> Vec<(&str, &str)> {
    let mut out = Vec::new();
    let mut offset = 0;
    for (i, piece) in text.split('"').enumerate() {
        if i % 2 == 1 {
            out.push((piece, &text[..offset - 1]));
        }
        offset += piece.len() + 1;
    }
    out
}

/// The codes the sources emit, outside the unit tests: the literal after the
/// severity argument of a `diagnostic(` or `report(` call, and the literals of
/// a `match` whose arm is followed by such a call (the unknown-name codes).
/// Messages contain spaces, so they never look like codes.
fn codes_in_sources() -> BTreeSet<String> {
    let mut codes = BTreeSet::new();
    let dir = root().join("crates/gsql-lsp/src/features");
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        let source = fs::read_to_string(&path).unwrap();
        let code = source.split("#[cfg(test)]").next().unwrap();
        for call in ["diagnostic(", "report("] {
            for (at, _) in code.match_indices(call) {
                if code[..at].trim_end().ends_with("fn") {
                    continue;
                }
                for (text, before) in literals(window(code, at, 400)) {
                    let before = before.trim_end().strip_suffix(',').unwrap_or("").trim_end();
                    if is_code(text) && (before.ends_with("level") || before.contains("severity")) {
                        codes.insert(text.to_string());
                        break;
                    }
                }
            }
        }
        for (at, _) in code.match_indices("=> \"") {
            let rest = &code[at + 4..];
            let Some(close) = rest.find('"') else { continue };
            let text = &rest[..close];
            if text.contains('-') && is_code(text) && window(rest, close, 300).contains("diagnostic(") {
                codes.insert(text.to_string());
            }
        }
    }
    codes
}

fn codes_in_docs() -> Vec<String> {
    let docs = fs::read_to_string(root().join("docs/diagnostics.md")).unwrap();
    docs.lines()
        .filter_map(|line| line.strip_prefix("| `"))
        .filter_map(|rest| rest.split('`').next())
        .map(str::to_string)
        .collect()
}

#[test]
fn every_diagnostic_code_has_a_row_in_the_docs() {
    let documented: BTreeSet<String> = codes_in_docs().into_iter().collect();
    let missing: Vec<_> = codes_in_sources().difference(&documented).cloned().collect();
    assert!(missing.is_empty(), "codes without a row in docs/diagnostics.md: {missing:?}");
}

#[test]
fn the_docs_name_only_existing_codes_once() {
    let sources = codes_in_sources();
    let docs = codes_in_docs();
    let unknown: Vec<_> = docs.iter().filter(|c| !sources.contains(*c)).collect();
    assert!(unknown.is_empty(), "documented codes the sources never emit: {unknown:?}");
    let unique: BTreeSet<_> = docs.iter().collect();
    assert_eq!(unique.len(), docs.len(), "a code has more than one row: {docs:?}");
}

#[test]
fn the_scan_finds_the_known_codes() {
    let codes = codes_in_sources();
    for code in [
        "syntax-error",
        "unused",
        "v3-comparison",
        "reserved-word",
        "accumulator-case",
        "value-count",
        "deprecated",
        "unknown-function",
        "unknown-method",
        "unknown-type-name",
    ] {
        assert!(codes.contains(code), "scan lost {code}: {codes:?}");
    }
}
