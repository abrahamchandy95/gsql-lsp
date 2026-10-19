//! The formatter must only change whitespace (and keyword case when asked),
//! keep files free of syntax errors, and be idempotent. Checked on the
//! examples, the test fixtures and every tree-sitter corpus test.

use std::path::{Path, PathBuf};

use gsql_lsp::features::KeywordCase;
use gsql_lsp::format::format_text;

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn gsql_files(dir: &Path, out: &mut Vec<(String, String)>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            gsql_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "gsql") {
            out.push((
                path.display().to_string(),
                std::fs::read_to_string(&path).unwrap(),
            ));
        }
    }
}

/// The source of each test in a tree-sitter corpus file.
fn corpus_sources(out: &mut Vec<(String, String)>) {
    let separator = "=".repeat(80);
    let divider = "-".repeat(80);
    let dir = repo().join("tree-sitter-gsql/test/corpus");
    for entry in std::fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        // ==== title ==== source ---- tree, repeated.
        let parts: Vec<&str> = text
            .split(&format!("{separator}\n"))
            .collect();
        for pair in parts[1..].chunks(2) {
            // `:error` cases are meant not to parse.
            if let [title, body] = pair
                && !title
                    .lines()
                    .any(|line| line.trim() == ":error")
            {
                let source = body
                    .split(&format!("\n{divider}\n"))
                    .next()
                    .unwrap_or("")
                    .trim();
                out.push((
                    format!("{}: {}", path.display(), title.trim()),
                    format!("{source}\n"),
                ));
            }
        }
    }
}

fn without_whitespace(text: &str) -> String {
    text.chars()
        .filter(|c| !c.is_whitespace())
        .collect()
}

#[test]
fn formatting_only_changes_layout_and_is_idempotent() {
    let mut inputs = Vec::new();
    gsql_files(&repo().join("examples"), &mut inputs);
    gsql_files(
        &Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures"),
        &mut inputs,
    );
    corpus_sources(&mut inputs);
    assert!(inputs.len() > 60, "found only {} inputs", inputs.len());
    for (name, text) in &inputs {
        for case in [
            KeywordCase::Preserve,
            KeywordCase::Upper,
            KeywordCase::Lower,
        ] {
            let Some(once) = format_text(text, case, 2) else {
                panic!("{name}: has syntax errors");
            };
            let expected = without_whitespace(text);
            let actual = without_whitespace(&once);
            match case {
                KeywordCase::Preserve => assert_eq!(
                    actual, expected,
                    "{name}: formatting changed more than layout"
                ),
                _ => assert!(
                    actual.eq_ignore_ascii_case(&expected),
                    "{name}: {case:?} changed more than case"
                ),
            }
            let twice = format_text(&once, case, 2).unwrap_or_else(|| {
                panic!("{name}: formatting broke the syntax")
            });
            assert_eq!(
                twice, once,
                "{name}: formatting with {case:?} is not idempotent"
            );
        }
    }
}

/// The GSQL Style Guide: lines over 80 characters are split and the extra
/// lines indented; a long header gets one parameter (or tuple field) per line.
#[test]
fn long_headers_and_tuples_get_one_item_per_line() {
    let input = "\
CREATE OR REPLACE QUERY retrieve_staleflag_as_of(DATETIME version_at, STRING rows_json,
                      STRING start_date=\"\", STRING end_date=\"\",
                      INT date_offset=0, INT date_limit=0) FOR GRAPH G2N SYNTAX v2 {
    TYPEDEF TUPLE<section STRING, grain STRING, grain_id STRING, measure_name STRING,
                  fiscal_date STRING, system_value DOUBLE, user_value DOUBLE,
                  has_user_value INT, final_value DOUBLE, is_editable INT,
                  version STRING, stale INT> GridCell;
    ListAccum<GridCell> @@cells;
    PRINT @@cells;
}
";
    let expected = "\
CREATE OR REPLACE QUERY retrieve_staleflag_as_of(
    DATETIME version_at,
    STRING rows_json,
    STRING start_date=\"\",
    STRING end_date=\"\",
    INT date_offset=0,
    INT date_limit=0
) FOR GRAPH G2N SYNTAX v2 {
    TYPEDEF TUPLE<
        section STRING,
        grain STRING,
        grain_id STRING,
        measure_name STRING,
        fiscal_date STRING,
        system_value DOUBLE,
        user_value DOUBLE,
        has_user_value INT,
        final_value DOUBLE,
        is_editable INT,
        version STRING,
        stale INT
    > GridCell;
    ListAccum<GridCell> @@cells;
    PRINT @@cells;
}
";
    let once = format_text(input, KeywordCase::Preserve, 4).unwrap();
    assert_eq!(once, expected);
    assert_eq!(format_text(&once, KeywordCase::Preserve, 4).unwrap(), once);
    // With 2 spaces per level the items go 2 spaces in.
    let two = format_text(input, KeywordCase::Preserve, 2).unwrap();
    assert!(
        two.starts_with(
            "CREATE OR REPLACE QUERY retrieve_staleflag_as_of(\n  DATETIME version_at,\n"
        ),
        "{two}"
    );
    assert!(
        two.contains("\n  TYPEDEF TUPLE<\n    section STRING,\n"),
        "{two}"
    );
    assert!(two.contains("\n    stale INT\n  > GridCell;\n"), "{two}");
}

/// `PRINT` items and SELECT clauses written 2 spaces in, and the `;` that ends
/// a SELECT, get the same 4-space levels as the rest of the file.
#[test]
fn print_items_and_select_clauses_are_indented_by_the_width() {
    let input = "\
CREATE QUERY fetch_descendants(VERTEX<Item> item_id) FOR GRAPH Store {
    SetAccum<VERTEX<Item>> @@vertices;
    SetAccum<EDGE> @@edges;
    input_item = {item_id};
    IF TRUE THEN
        children = SELECT t
          FROM input_item:s - (E:e) - Item:t
          WHERE e.live == TRUE
            AND e.valid == TRUE
          ACCUM @@vertices += t, @@edges += e
        ;
        WHILE children.size() > 0 DO
            children = SELECT t
              FROM children:s - (E:e) - Item:t
              ACCUM @@vertices += t, @@edges += e
          ;
        END;
        PRINT
          @@vertices AS vertices,
          @@edges AS edges
        ;
    END;
}
";
    let expected = "\
CREATE QUERY fetch_descendants(VERTEX<Item> item_id) FOR GRAPH Store {
    SetAccum<VERTEX<Item>> @@vertices;
    SetAccum<EDGE> @@edges;
    input_item = {item_id};
    IF TRUE THEN
        children = SELECT t
            FROM input_item:s - (E:e) - Item:t
            WHERE e.live == TRUE
            AND e.valid == TRUE
            ACCUM @@vertices += t, @@edges += e
        ;
        WHILE children.size() > 0 DO
            children = SELECT t
                FROM children:s - (E:e) - Item:t
                ACCUM @@vertices += t, @@edges += e
            ;
        END;
        PRINT
            @@vertices AS vertices,
            @@edges AS edges
        ;
    END;
}
";
    let once = format_text(input, KeywordCase::Preserve, 4).unwrap();
    assert_eq!(once, expected);
    assert_eq!(format_text(&once, KeywordCase::Preserve, 4).unwrap(), once);
    for line in once.lines() {
        let indent = line.len() - line.trim_start().len();
        assert_eq!(indent % 4, 0, "{line:?}");
    }
}
