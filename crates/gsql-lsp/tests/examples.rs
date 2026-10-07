//! The example projects must parse and analyze without errors or warnings and
//! be formatted, and the syntax fixtures must parse without errors.

use std::path::{Path, PathBuf};

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn examples_have_no_errors_or_warnings() {
    let options = gsql_lsp::check::Options {
        paths: vec![repo().join("examples")],
        errors_only: false,
        format: gsql_lsp::check::OutputFormat::Text,
    };
    let mut output = Vec::new();
    let errors = gsql_lsp::check::run(&options, &mut output).unwrap().problems;
    let output = String::from_utf8(output).unwrap();
    let problems: Vec<&str> = output.lines().filter(|line| !line.contains(": hint: ")).collect();
    assert_eq!(errors, 0, "{output}");
    assert!(problems.is_empty(), "{}", problems.join("\n"));
}

#[test]
fn examples_follow_the_style_guide() {
    // Keywords in all caps and `//` comments: the style hints find nothing. The
    // indentation (4 spaces per level) is checked by `examples_are_formatted`.
    let options = gsql_lsp::check::Options {
        paths: vec![repo().join("examples")],
        errors_only: false,
        format: gsql_lsp::check::OutputFormat::Text,
    };
    let mut output = Vec::new();
    gsql_lsp::check::run(&options, &mut output).unwrap();
    let output = String::from_utf8(output).unwrap();
    let style: Vec<&str> =
        output.lines().filter(|line| line.contains("[keyword-case]") || line.contains("[hash-comment]")).collect();
    assert!(style.is_empty(), "{}", style.join("\n"));
}

#[test]
fn examples_are_formatted() {
    let indent = gsql_lsp::format::Options::default().indent;
    let mut stale = Vec::new();
    let mut pending = vec![repo().join("examples")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else if path.extension().is_some_and(|e| e == "gsql") {
                let text = std::fs::read_to_string(&path).unwrap();
                let formatted = gsql_lsp::format::format_text(&text, gsql_lsp::features::KeywordCase::Preserve, indent);
                if formatted.as_deref() != Some(text.as_str()) {
                    stale.push(path.display().to_string());
                }
            }
        }
    }
    assert!(stale.is_empty(), "run `gsql-lsp format examples`: {stale:?}");
}

#[test]
fn fixtures_parse_without_syntax_errors() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures");
    let mut parser = gsql_lsp::syntax::new_parser();
    let mut checked = 0;
    for entry in std::fs::read_dir(&dir).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|e| e == "gsql") {
            let text = std::fs::read_to_string(&path).unwrap();
            let tree = gsql_lsp::syntax::parse(&mut parser, &text, None);
            assert!(!tree.root_node().has_error(), "{} has syntax errors", path.display());
            checked += 1;
        }
    }
    assert!(checked >= 3);
}
