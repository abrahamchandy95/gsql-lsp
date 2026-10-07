//! Document links: `@file.gsql` includes of GSQL shell scripts, and data
//! files of loading jobs that exist next to the script.

use std::path::{Path, PathBuf};

use tree_sitter::Node;

use crate::features::Snapshot;
use crate::lsp::types::{DocumentLink, Location, Position, Range};
use crate::syntax;
use crate::text::Span;
use crate::uri;

/// The local file a path in this document refers to, if it exists. Relative
/// paths are tried against the document's directory, then the workspace roots.
fn resolve(snapshot: &Snapshot, path: &str) -> Option<PathBuf> {
    let path = Path::new(path);
    if path.is_absolute() {
        return path.is_file().then(|| path.to_path_buf());
    }
    let document_dir = uri::to_path(snapshot.uri).and_then(|p| p.parent().map(Path::to_path_buf));
    document_dir
        .into_iter()
        .chain(snapshot.workspace.roots.iter().cloned())
        .map(|dir| dir.join(path))
        .find(|candidate| candidate.is_file())
}

/// A path written in the document: the span of the path and its text.
fn path_of(node: Node, source: &str) -> Option<(Span, String)> {
    match node.kind() {
        // `@schema.gsql`
        "file_include" => {
            let text = syntax::text(node, source);
            Some((Span::new(node.start_byte() + 1, node.end_byte()), text[1..].to_string()))
        }
        // `DEFINE FILENAME f = "data/people.csv"`, `LOAD "data/people.csv" TO ...`,
        // `RUN LOADING JOB j USING f = "data/people.csv"`
        "string" => {
            let parent = node.parent()?;
            let is_path = match parent.kind() {
                "define_filename_statement" => syntax::field_name(node) == Some("path"),
                "load_statement" => syntax::field_name(node) == Some("source"),
                "option_assignment" => syntax::find_ancestor(parent, &["using_clause"])
                    .is_some_and(|u| u.parent().is_some_and(|p| p.kind() == "run_job_statement")),
                _ => false,
            };
            let text = syntax::text(node, source);
            let path = text.strip_prefix('"')?.strip_suffix('"')?;
            // Machine-qualified paths (`ALL:`, `ANY:`, `m1:`) live on the server.
            if !is_path || path.is_empty() || path.contains(':') || path.starts_with('$') {
                return None;
            }
            Some((Span::new(node.start_byte() + 1, node.end_byte() - 1), path.to_string()))
        }
        _ => None,
    }
}

pub fn document_links(snapshot: &Snapshot) -> Vec<DocumentLink> {
    let source = snapshot.text();
    let mut links = Vec::new();
    syntax::walk(snapshot.root(), |node| {
        let Some((span, path)) = path_of(node, source) else {
            return;
        };
        if let Some(file) = resolve(snapshot, &path) {
            links.push(DocumentLink {
                range: snapshot.range(span),
                target: Some(uri::from_path(&file)),
                tooltip: Some(file.display().to_string()),
            });
        }
    });
    links
}

/// Go to definition on an `@file` include opens the file.
pub fn definition(snapshot: &Snapshot, position: Position) -> Option<Location> {
    let offset = snapshot.offset(position);
    let node = snapshot.root().descendant_for_byte_range(offset, offset)?;
    let node = syntax::lineage(snapshot.root(), node).into_iter().find(|n| n.kind() == "file_include")?;
    let (_, path) = path_of(node, snapshot.text())?;
    let file = resolve(snapshot, &path)?;
    Some(Location { uri: uri::from_path(&file), range: Range::default() })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::Config;
    use crate::text::{PositionEncoding, SourceText};
    use crate::workspace::Workspace;

    #[test]
    fn links_includes_and_local_data_files() {
        let dir = std::env::temp_dir().join(format!("gsql-links-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("data")).unwrap();
        std::fs::write(dir.join("schema.gsql"), "CREATE VERTEX P (PRIMARY_ID id STRING)\n").unwrap();
        std::fs::write(dir.join("data/people.csv"), "id\n").unwrap();
        let text = "@schema.gsql\n@missing.gsql\nCREATE LOADING JOB j FOR GRAPH g {\n  DEFINE FILENAME f = \"data/people.csv\";\n  DEFINE FILENAME g = \"ANY:/data/x.csv\";\n}\n";
        let tree = syntax::parse(&mut syntax::new_parser(), text, None);
        let analysis = crate::analysis::analyze(&tree, text);
        let source = SourceText::new(text.to_string());
        let workspace = Workspace::default();
        let config = Config::default();
        let uri = uri::from_path(&dir.join("main.gsql"));
        let snapshot = Snapshot {
            uri: &uri,
            source: &source,
            tree: &tree,
            analysis: &analysis,
            workspace: &workspace,
            encoding: PositionEncoding::Utf16,
            config: &config,
        };
        let links = document_links(&snapshot);
        let targets: Vec<String> = links.iter().filter_map(|l| l.target.clone()).collect();
        assert_eq!(targets, [uri::from_path(&dir.join("schema.gsql")), uri::from_path(&dir.join("data/people.csv"))]);
        assert_eq!(links[0].range, Range::new(Position::new(0, 1), Position::new(0, 12)));
        let location = definition(&snapshot, Position::new(0, 3)).unwrap();
        assert_eq!(location.uri, uri::from_path(&dir.join("schema.gsql")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
