//! Hints for deviations from TigerGraph's GSQL Style Guide (an appendix of the
//! language reference). They only nudge: each one is a hint, carries a quick fix
//! that is safe to apply without review (so "fix all" applies it too), and
//! `diagnostics.style` turns them all off.
//!
//! * "Always write keywords and reserved words in all caps": `keyword-case`.
//! * "Use // for single line and inline comments", "Do not use #": `hash-comment`.
//!
//! Indentation (4 spaces, no tabs) is the formatter's job, see `formatting`.

use crate::features::diagnostics::{add_fix, diagnostic};
use crate::features::formatting::keyword_case_edits;
use crate::features::{KeywordCase, Snapshot};
use crate::lsp::types::{Diagnostic, TextEdit, severity};
use crate::syntax;
use crate::text::Span;

pub fn check(snapshot: &Snapshot) -> Vec<Diagnostic> {
    let mut out = Vec::new();
    keyword_case(snapshot, &mut out);
    hash_comments(snapshot, &mut out);
    out
}

/// Keywords and reserved words in all caps. `TRUE`, `FALSE` and `NULL` are
/// reserved words, so they count; the formatter's `keywordCase` decides which
/// tokens are keywords, so the hints and `format` agree.
fn keyword_case(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    for (start, end, upper) in keyword_case_edits(snapshot, KeywordCase::Upper) {
        let message = format!("Keywords and reserved words are written in all caps: `{upper}`");
        let mut hint = diagnostic(snapshot, Span::new(start, end), severity::HINT, "keyword-case", message);
        let edit = TextEdit { range: hint.range, new_text: upper.clone() };
        add_fix(&mut hint, format!("Change to `{upper}`"), vec![edit], true);
        out.push(hint);
    }
}

/// `#` comments: the style guide writes single-line comments with `//` (and the
/// coming GQL standard has no `#`). The hint covers the `#` only.
fn hash_comments(snapshot: &Snapshot, out: &mut Vec<Diagnostic>) {
    let source = snapshot.text();
    syntax::walk(snapshot.root(), |node| {
        if node.kind() != "comment" || !syntax::text(node, source).starts_with('#') {
            return;
        }
        let hash = Span::new(node.start_byte(), node.start_byte() + 1);
        let message = "Comments start with `//`, not `#`; multi-line comments go between `/*` and `*/`".to_string();
        let mut hint = diagnostic(snapshot, hash, severity::HINT, "hash-comment", message);
        let edit = TextEdit { range: hint.range, new_text: "//".to_string() };
        add_fix(&mut hint, "Change `#` to `//`", vec![edit], true);
        out.push(hint);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::code_actions::code_actions;
    use crate::features::diagnostics::diagnostics;
    use crate::features::test_support::Fixture;
    use crate::lsp::types::Range;
    use crate::text::{PositionEncoding, SourceText};

    fn fixture(text: &str) -> Fixture {
        let mut fixture = Fixture::new(text);
        fixture.config.diagnostics_style = true;
        fixture
    }

    fn hints(text: &str) -> Vec<Diagnostic> {
        check(&fixture(text).snapshot())
    }

    /// `text` with the edits applied, right to left.
    fn apply(text: &str, mut edits: Vec<TextEdit>) -> String {
        let source = SourceText::new(text.to_string());
        let offset = |p| source.offset(p, PositionEncoding::Utf16);
        edits.sort_by_key(|e| std::cmp::Reverse((e.range.start, e.range.end)));
        let mut result = text.to_string();
        for e in edits {
            result.replace_range(offset(e.range.start)..offset(e.range.end), &e.new_text);
        }
        result
    }

    /// The first fix attached to each hint.
    fn fix_edits(hints: &[Diagnostic]) -> Vec<TextEdit> {
        hints
            .iter()
            .flat_map(|h| {
                let fix = &h.data.as_ref().expect("a fix")["fixes"][0];
                assert_eq!(fix["safe"], true, "style fixes are certain");
                serde_json::from_value::<Vec<TextEdit>>(fix["edits"].clone()).unwrap()
            })
            .collect()
    }

    #[test]
    fn keywords_in_lower_case_get_a_hint_with_a_fix() {
        let text = "create query q() {\n  print 1;\n}\n";
        let found = hints(text);
        let words: Vec<(&str, &str)> = found.iter().map(|h| (h.code.as_deref().unwrap(), h.message.as_str())).collect();
        assert_eq!(
            words,
            [
                ("keyword-case", "Keywords and reserved words are written in all caps: `CREATE`"),
                ("keyword-case", "Keywords and reserved words are written in all caps: `QUERY`"),
                ("keyword-case", "Keywords and reserved words are written in all caps: `PRINT`"),
            ]
        );
        assert!(found.iter().all(|h| h.severity == Some(severity::HINT)));
        assert_eq!(
            found[1].range,
            Range::new(crate::lsp::types::Position::new(0, 7), crate::lsp::types::Position::new(0, 12))
        );
        assert_eq!(apply(text, fix_edits(&found)), "CREATE QUERY q() {\n  PRINT 1;\n}\n");
    }

    #[test]
    fn mixed_case_counts_and_so_do_the_reserved_literals() {
        let text = "Create Query q() {\n  BOOL a = true;\n  BOOL b = False;\n  STRING s = null;\n}\n";
        assert_eq!(
            apply(text, fix_edits(&hints(text))),
            "CREATE QUERY q() {\n  BOOL a = TRUE;\n  BOOL b = FALSE;\n  STRING s = NULL;\n}\n"
        );
    }

    #[test]
    fn upper_case_code_gets_no_hint_and_names_strings_and_comments_are_not_keywords() {
        let text = "CREATE QUERY q() {\n  INT select_count = 1; // select from where\n  /* print */\n  PRINT \"select from\", select_count;\n}\n";
        assert!(hints(text).is_empty(), "{:?}", hints(text));
        // Accumulator type names are case sensitive and not keywords.
        assert!(hints("CREATE QUERY q() {\n  SumAccum<INT> @@n;\n  PRINT @@n;\n}\n").is_empty());
    }

    #[test]
    fn hash_comments_get_a_hint_on_the_hash() {
        let text =
            "# note\nCREATE QUERY q() {\n  PRINT 1; # trailing\n  // fine\n  /* # fine */\n  PRINT \"# fine\";\n}\n";
        let found = hints(text);
        assert_eq!(found.len(), 2, "{found:?}");
        assert!(found.iter().all(|h| h.code.as_deref() == Some("hash-comment") && h.severity == Some(severity::HINT)));
        // The hint covers the `#` only.
        assert!(found.iter().all(|h| h.range.start.line == h.range.end.line && h.range.end.character == h.range.start.character + 1));
        assert_eq!(
            apply(text, fix_edits(&found)),
            "// note\nCREATE QUERY q() {\n  PRINT 1; // trailing\n  // fine\n  /* # fine */\n  PRINT \"# fine\";\n}\n"
        );
    }

    #[test]
    fn the_hints_are_part_of_the_diagnostics_unless_switched_off() {
        let text = "CREATE QUERY q() {\n  print 1; # c\n}\n";
        let mut fixture = fixture(text);
        let codes = |fixture: &Fixture| -> Vec<String> {
            diagnostics(&fixture.snapshot()).into_iter().filter_map(|d| d.code).collect()
        };
        assert_eq!(codes(&fixture), ["keyword-case", "hash-comment"]);
        fixture.config.update(&serde_json::json!({ "diagnostics": { "style": false } }));
        assert!(codes(&fixture).is_empty());
        // On by default.
        assert!(crate::features::Config::default().diagnostics_style);
    }

    #[test]
    fn lines_with_syntax_errors_get_no_style_hints() {
        // The broken line is probably misread; its hints would be guesses.
        let text = "create query q() {\n  print ;\n}\n";
        let found = diagnostics(&fixture(text).snapshot());
        let hints: Vec<u32> =
            found.iter().filter(|d| d.code.as_deref() == Some("keyword-case")).map(|d| d.range.start.line).collect();
        assert_eq!(hints, [0, 0], "{found:?}");
    }

    #[test]
    fn fix_all_applies_the_style_fixes() {
        let text = "create query q() {\n  print 1; # c\n}\n";
        let fixture = fixture(text);
        let snapshot = fixture.snapshot();
        let found = diagnostics(&snapshot);
        let whole = Range::new(snapshot.position(0), snapshot.position(text.len()));
        let only = ["source.fixAll".to_string()];
        let actions = code_actions(&snapshot, whole, &found, Some(&only), &found);
        assert_eq!(actions.len(), 1);
        let edits = actions[0].edit.changes[snapshot.uri].clone();
        assert_eq!(apply(text, edits), "CREATE QUERY q() {\n  PRINT 1; // c\n}\n");
    }

    #[test]
    fn each_hint_offers_its_quick_fix() {
        let text = "create query q() {\n  PRINT 1; # c\n}\n";
        let fixture = fixture(text);
        let snapshot = fixture.snapshot();
        let found = diagnostics(&snapshot);
        let only = ["quickfix".to_string()];
        let at = |offset: usize| Range::new(snapshot.position(offset), snapshot.position(offset));
        // The cursor in `create`: one fix, the preferred one.
        let actions = code_actions(&snapshot, at(2), &found, Some(&only), &found);
        assert_eq!(actions.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Change to `CREATE`"]);
        assert_eq!(actions[0].is_preferred, Some(true));
        let edits = actions[0].edit.changes[snapshot.uri].clone();
        assert_eq!(apply(text, edits), "CREATE query q() {\n  PRINT 1; # c\n}\n");
        // On the `#`.
        let actions = code_actions(&snapshot, at(text.find('#').unwrap()), &found, Some(&only), &found);
        assert_eq!(actions.iter().map(|a| a.title.as_str()).collect::<Vec<_>>(), ["Change `#` to `//`"]);
        let edits = actions[0].edit.changes[snapshot.uri].clone();
        assert_eq!(apply(text, edits), "create query q() {\n  PRINT 1; // c\n}\n");
    }
}
