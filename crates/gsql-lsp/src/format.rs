//! `gsql-lsp format`: format files on the command line with the same
//! formatter the language server uses.

use std::io::{Read, Write};
use std::path::PathBuf;

use crate::check::Summary;
use crate::features::{Config, KeywordCase, Snapshot};
use crate::lsp::types::{FormattingOptions, TextEdit};
use crate::text::{PositionEncoding, SourceText};
use crate::workspace::{self, Workspace};
use crate::{analysis, syntax};

pub struct Options {
    /// Files or directories; `-` formats standard input to standard output.
    pub paths: Vec<PathBuf>,
    /// Report files that are not formatted instead of rewriting them.
    pub check: bool,
    pub keyword_case: KeywordCase,
    /// Spaces per indentation level; the GSQL Style Guide says 4.
    pub indent: u32,
}

impl Default for Options {
    fn default() -> Self {
        Options { paths: Vec::new(), check: false, keyword_case: KeywordCase::Preserve, indent: 4 }
    }
}

/// The formatted text, or `None` when the text has syntax errors (the
/// formatter leaves broken files alone).
pub fn format_text(text: &str, keyword_case: KeywordCase, indent: u32) -> Option<String> {
    let tree = syntax::parse(&mut syntax::new_parser(), text, None);
    if tree.root_node().has_error() {
        return None;
    }
    let analysis = analysis::analyze(&tree, text);
    let source = SourceText::new(text.to_string());
    let workspace = Workspace::default();
    let config = Config { format_keyword_case: keyword_case, ..Config::default() };
    let snapshot = Snapshot {
        uri: "file:///stdin.gsql",
        source: &source,
        tree: &tree,
        analysis: &analysis,
        workspace: &workspace,
        encoding: PositionEncoding::Utf8,
        config: &config,
    };
    let options = FormattingOptions { tab_size: indent.max(1), ..FormattingOptions::default() };
    let edits = crate::features::formatting::format(&snapshot, &options, None);
    Some(apply(&source, &edits))
}

fn apply(source: &SourceText, edits: &[TextEdit]) -> String {
    let mut spans: Vec<(usize, usize, &str)> = edits
        .iter()
        .map(|e| {
            let start = source.offset(e.range.start, PositionEncoding::Utf8);
            let end = source.offset(e.range.end, PositionEncoding::Utf8);
            (start, end, e.new_text.as_str())
        })
        .collect();
    spans.sort_by_key(|&(start, _, _)| std::cmp::Reverse(start));
    let mut text = source.text.clone();
    for (start, end, new_text) in spans {
        text.replace_range(start..end, new_text);
    }
    text
}

/// The file's text; a file that is not UTF-8 is refused (formatting would
/// have to rewrite its bytes), with the way out in the message.
fn read_utf8(path: &std::path::Path) -> std::io::Result<String> {
    decode_utf8(std::fs::read(path)?)
}

fn decode_utf8(bytes: Vec<u8>) -> std::io::Result<String> {
    String::from_utf8(bytes).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "not valid UTF-8; convert it first (e.g. iconv -f latin1 -t utf-8)",
        )
    })
}

/// Formats the files, counting those not formatted (with `check`) or with
/// syntax errors as problems, and those that could not be read or written as
/// unreadable (reported on stderr; the other files are still processed).
pub fn run(options: &Options, out: &mut impl Write) -> std::io::Result<Summary> {
    let mut problems = 0;
    let mut unreadable = 0;
    if options.paths.iter().any(|p| p.as_os_str() == "-") {
        let mut bytes = Vec::new();
        std::io::stdin().read_to_end(&mut bytes)?;
        let text = decode_utf8(bytes)?;
        match format_text(&text, options.keyword_case, options.indent) {
            Some(formatted) if options.check => {
                if formatted != text {
                    writeln!(out, "<stdin>")?;
                    problems += 1;
                }
            }
            Some(formatted) => write!(out, "{formatted}")?,
            None => {
                // Pass broken input through unchanged so editors keep the
                // text (a check only reports).
                if options.check {
                    writeln!(out, "<stdin>")?;
                } else {
                    write!(out, "{text}")?;
                }
                eprintln!("gsql-lsp: <stdin> has syntax errors; left unchanged");
                problems += 1;
            }
        }
    }
    let mut files = Vec::new();
    for path in options.paths.iter().filter(|p| p.as_os_str() != "-") {
        if path.is_dir() {
            files.extend(workspace::scan(std::slice::from_ref(path)));
        } else {
            files.push(path.clone());
        }
    }
    for path in files {
        let text = match read_utf8(&path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("{}: error: cannot read file: {err}", crate::check::display(&path));
                unreadable += 1;
                continue;
            }
        };
        match format_text(&text, options.keyword_case, options.indent) {
            Some(formatted) if formatted == text => {}
            Some(formatted) => {
                if options.check {
                    writeln!(out, "{}", crate::check::display(&path))?;
                    problems += 1;
                } else {
                    if let Err(err) = std::fs::write(&path, formatted) {
                        eprintln!("{}: error: cannot write file: {err}", crate::check::display(&path));
                        unreadable += 1;
                    }
                }
            }
            None => {
                if options.check {
                    writeln!(out, "{}", crate::check::display(&path))?;
                }
                eprintln!("gsql-lsp: {} has syntax errors; left unchanged", crate::check::display(&path));
                problems += 1;
            }
        }
    }
    Ok(Summary { problems, unreadable })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_text() {
        let formatted = format_text("create query q() {\nprint 1;\n}", KeywordCase::Upper, 4).unwrap();
        assert_eq!(formatted, "CREATE QUERY q() {\n    PRINT 1;\n}\n");
        assert_eq!(format_text("CREATE QUERY q() {", KeywordCase::Preserve, 2), None);
    }

    #[test]
    fn indents_by_four_spaces_unless_told_otherwise() {
        let options = Options::default();
        assert_eq!(options.indent, 4);
        let text = "CREATE QUERY q() {\nPRINT 1;\n}\n";
        assert_eq!(
            format_text(text, options.keyword_case, options.indent).unwrap(),
            "CREATE QUERY q() {\n    PRINT 1;\n}\n"
        );
        assert_eq!(format_text(text, options.keyword_case, 2).unwrap(), "CREATE QUERY q() {\n  PRINT 1;\n}\n");
    }

    #[test]
    fn non_utf8_input_gets_the_actionable_message() {
        let err = decode_utf8(b"// caf\xe9".to_vec()).unwrap_err();
        assert!(err.to_string().contains("convert it first (e.g. iconv -f latin1 -t utf-8)"));
        assert_eq!(decode_utf8("ok".into()).unwrap(), "ok");
    }

    #[test]
    fn check_lists_files_with_syntax_errors() {
        let dir = std::env::temp_dir().join(format!("gsql-format-check-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("bad.gsql"), "CREATE QUERY q() {").unwrap();
        std::fs::write(dir.join("ok.gsql"), "CREATE QUERY q() {\n    PRINT 1;\n}\n").unwrap();
        let options = Options { paths: vec![dir.clone()], check: true, ..Options::default() };
        let mut out = Vec::new();
        let summary = run(&options, &mut out).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let out = String::from_utf8(out).unwrap();
        assert!(out.contains("bad.gsql") && !out.contains("ok.gsql"), "{out}");
        assert_eq!(summary, Summary { problems: 1, unreadable: 0 });
    }

    #[test]
    fn non_utf8_files_are_refused_with_a_way_out() {
        let path = std::env::temp_dir().join(format!("gsql-format-latin1-{}.gsql", std::process::id()));
        std::fs::write(&path, b"PRINT \"caf\xe9\";").unwrap();
        let err = read_utf8(&path).unwrap_err();
        std::fs::remove_file(&path).unwrap();
        assert!(err.to_string().contains("not valid UTF-8; convert it first (e.g. iconv -f latin1 -t utf-8)"));
    }

    #[test]
    fn unreadable_files_do_not_stop_the_run() {
        let dir = std::env::temp_dir().join(format!("gsql-format-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.gsql"), "CREATE QUERY a() {\nPRINT 1;\n}\n").unwrap();
        std::fs::write(dir.join("b.gsql"), [0xc3, 0x28]).unwrap();
        std::fs::write(dir.join("z.gsql"), "CREATE QUERY z() {\nPRINT 1;\n}\n").unwrap();
        let options = Options { paths: vec![dir.clone()], ..Options::default() };
        let summary = run(&options, &mut Vec::new()).unwrap();
        let z = std::fs::read_to_string(dir.join("z.gsql")).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(summary, Summary { problems: 0, unreadable: 1 });
        assert_eq!(z, "CREATE QUERY z() {\n    PRINT 1;\n}\n");
    }
}
