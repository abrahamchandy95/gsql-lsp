//! Smart selection: expand to the enclosing syntax node.

use crate::features::Snapshot;
use crate::lsp::types::{Position, Range, SelectionRange};
use crate::syntax;
use crate::text::Span;

pub fn selection_ranges(
    snapshot: &Snapshot,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|&position| selection_range_at(snapshot, position))
        .collect()
}

/// The node at `position`, then each larger node up to the root.
fn selection_range_at(
    snapshot: &Snapshot,
    position: Position,
) -> SelectionRange {
    let root = snapshot.root();
    let Some(leaf) = syntax::leaf_at(root, snapshot.offset(position)) else {
        return SelectionRange {
            range: Range::new(position, position),
            parent: None,
        };
    };
    let mut spans: Vec<Span> = syntax::lineage(root, leaf)
        .into_iter()
        .map(Span::of)
        .collect();
    // A wrapper node has its child's span.
    spans.dedup();
    spans
        .into_iter()
        .rfold(None, |parent, span| {
            Some(SelectionRange {
                range: snapshot.range(span),
                parent: parent.map(Box::new),
            })
        })
        .expect("the lineage includes the root")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{Fixture, cursor};

    #[test]
    fn expands_from_identifier_to_file() {
        let (text, offset) = cursor("CREATE QUERY q() { PRINT a + |b; }");
        let fixture = Fixture::new(&text);
        let snapshot = fixture.snapshot();
        let selection =
            &selection_ranges(&snapshot, &[snapshot.position(offset)])[0];
        assert_eq!(
            selection.range,
            Range::new(Position::new(0, 29), Position::new(0, 30))
        );
        let parent = selection.parent.as_ref().unwrap();
        assert_eq!(
            parent.range,
            Range::new(Position::new(0, 25), Position::new(0, 30))
        );
    }
}
