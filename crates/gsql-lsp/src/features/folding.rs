//! Folding ranges for blocks, clauses and comments.

use crate::features::Snapshot;
use crate::lsp::types::FoldingRange;
use crate::syntax;

const FOLDABLE: &[&str] = &[
    "query_body",
    "opencypher_body",
    "loading_job_body",
    "schema_change_body",
    "vertex_attribute_list",
    "edge_attribute_list",
    "parameter_list",
    "select_statement",
    "if_statement",
    "else_if_clause",
    "else_clause",
    "case_statement",
    "when_clause",
    "while_statement",
    "foreach_statement",
    "try_statement",
    "exception_handler",
    "accum_clause",
    "post_accum_clause",
    "vertex_set_literal",
    "list_literal",
    "argument_list",
    "stub_object",
];

pub fn folding_ranges(snapshot: &Snapshot) -> Vec<FoldingRange> {
    let mut ranges = Vec::new();
    let mut comment_run: Option<(u32, u32)> = None;
    let flush = |run: &mut Option<(u32, u32)>,
                 ranges: &mut Vec<FoldingRange>| {
        if let Some((start, end)) = run.take()
            && end > start
        {
            ranges.push(FoldingRange {
                start_line: start,
                end_line: end,
                kind: Some("comment"),
            });
        }
    };
    let source = snapshot.text();
    let lines = &snapshot.source.lines;
    // Lines from byte offsets: tree-sitter rows ignore a lone `\r`.
    let line = |offset: usize| lines.line_of(offset) as u32;
    syntax::walk(snapshot.root(), |node| {
        let start = line(node.start_byte());
        let end = line(node.end_byte());
        if node.kind() == "comment" {
            let is_line_comment =
                !syntax::text(node, source).starts_with("/*");
            if is_line_comment {
                match &mut comment_run {
                    Some((_, run_end)) if *run_end + 1 == start => {
                        *run_end = end
                    }
                    _ => {
                        flush(&mut comment_run, &mut ranges);
                        comment_run = Some((start, end));
                    }
                }
            } else if end > start {
                ranges.push(FoldingRange {
                    start_line: start,
                    end_line: end,
                    kind: Some("comment"),
                });
            }
            return;
        }
        if !FOLDABLE.contains(&node.kind()) || end <= start {
            return;
        }
        // Keep a closing `}`, `)`, `]` or END visible when it starts its own line.
        let mut last_line = end;
        if let Some(last) = node.child(node.child_count().saturating_sub(1)) {
            let closes = matches!(last.kind(), "}" | ")" | "]" | "END");
            let last_row = line(last.start_byte());
            let line_start = lines.line_start(last_row as usize);
            let only_whitespace_before = source
                [line_start..last.start_byte()]
                .trim()
                .is_empty();
            if closes && only_whitespace_before && last_row > start {
                last_line = last_row - 1;
            }
        }
        // Clauses that end where the next clause begins should not hide it.
        if matches!(
            node.kind(),
            "else_if_clause"
                | "else_clause"
                | "when_clause"
                | "exception_handler"
        ) && node
            .next_sibling()
            .is_some_and(|n| line(n.start_byte()) == end)
        {
            last_line = last_line.min(end.saturating_sub(1));
        }
        if last_line > start {
            ranges.push(FoldingRange {
                start_line: start,
                end_line: last_line,
                kind: None,
            });
        }
    });
    flush(&mut comment_run, &mut ranges);
    ranges.sort_by_key(|r| (r.start_line, r.end_line));
    ranges.dedup_by(|a, b| {
        a.start_line == b.start_line && a.end_line == b.end_line
    });
    ranges
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::Fixture;

    #[test]
    fn folds_query_bodies_and_comment_runs() {
        let fixture = Fixture::new(
            "// a\n// b\nCREATE QUERY q() {\n  PRINT 1;\n  PRINT 2;\n}\n",
        );
        let ranges = folding_ranges(&fixture.snapshot());
        assert!(ranges.contains(&FoldingRange {
            start_line: 0,
            end_line: 1,
            kind: Some("comment")
        }));
        assert!(
            ranges.contains(&FoldingRange {
                start_line: 2,
                end_line: 4,
                kind: None
            }),
            "{ranges:?}"
        );
    }
}
