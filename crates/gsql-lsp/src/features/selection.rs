//! Smart selection: expand to the enclosing syntax node.

use crate::features::Snapshot;
use crate::lsp::types::{Position, Range, SelectionRange};
use crate::syntax;

pub fn selection_ranges(
    snapshot: &Snapshot,
    positions: &[Position],
) -> Vec<SelectionRange> {
    positions
        .iter()
        .map(|&position| {
            let offset = snapshot.offset(position);
            let mut ranges: Vec<Range> = Vec::new();
            if let Some(node) = syntax::leaf_at(snapshot.root(), offset) {
                for ancestor in syntax::lineage(snapshot.root(), node) {
                    let range =
                        snapshot.range(crate::text::Span::of(ancestor));
                    if ranges.last() != Some(&range) {
                        ranges.push(range);
                    }
                }
            }
            if ranges.is_empty() {
                ranges.push(Range::new(position, position));
            }
            let mut selection: Option<SelectionRange> = None;
            for range in ranges.into_iter().rev() {
                selection = Some(SelectionRange {
                    range,
                    parent: selection.map(Box::new),
                });
            }
            selection.expect("at least one range")
        })
        .collect()
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
