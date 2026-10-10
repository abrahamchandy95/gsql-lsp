//! The workspace index: schema objects, queries and jobs across all GSQL
//! files, plus the references to them, for cross-file navigation.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use tree_sitter::Tree;

use crate::analysis::{
    Analysis, EdgeEnds, FILE_SCOPE, Param, REVERSE_EDGE_PREFIX, Role,
    SymbolKind, Ty,
};
use crate::features::{autocorrect, plural};
use crate::lsp::types::{Location, Range};
use crate::text::{PositionEncoding, SourceText};
use crate::uri;

pub const EXTENSIONS: &[&str] = &["gsql", "gsq"];

/// Marks the project root for a file outside every workspace folder.
const ROOT_MARKER: &str = ".gsqlroot";
const MAX_FILES: usize = 10_000;
/// Stop scanning after this many directories (e.g. a home directory opened as a workspace).
const MAX_DIRECTORIES: usize = 50_000;
const MAX_FILE_BYTES: u64 = 8 * 1024 * 1024;
const SKIPPED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "build",
    "dist",
    "venv",
    "__pycache__",
];

/// A workspace-level declaration.
#[derive(Debug, Clone)]
pub struct GlobalSymbol {
    pub uri: String,
    pub name: String,
    pub kind: SymbolKind,
    pub owner: Option<String>,
    pub range: Range,
    pub selection: Range,
    pub detail: String,
    pub doc: Option<String>,
    pub ty: Ty,
    pub graph: Option<String>,
    pub params: Vec<Param>,
    /// The RETURNS type of a query as written.
    pub returns: Option<String>,
    pub members: Vec<String>,
    pub ends: EdgeEnds,
    pub vector: bool,
    pub id_only: bool,
    pub strict_create: bool,
    pub altered: bool,
    pub in_error: bool,
}

impl GlobalSymbol {
    pub fn location(&self) -> Location {
        Location {
            uri: self.uri.clone(),
            range: self.selection,
        }
    }

    /// Whether this is a file variable of a loading job.
    pub fn is_job_file(&self) -> bool {
        self.kind == SymbolKind::FilenameVariable && self.owner.is_some()
    }

    /// Whether this edge type is declared by another's `WITH REVERSE_EDGE`.
    pub fn is_reverse_edge(&self) -> bool {
        self.kind == SymbolKind::EdgeType
            && self.detail.starts_with(REVERSE_EDGE_PREFIX)
    }
}

/// An occurrence of a name that may refer to a workspace-level declaration.
#[derive(Debug, Clone)]
pub struct GlobalReference {
    pub name: String,
    /// Kinds of declaration the name can refer to in this position.
    pub kinds: Vec<SymbolKind>,
    /// For attributes: the possible owning types (empty when unknown).
    pub owners: Vec<String>,
    /// A call by plain name: `name(..)`.
    pub call: bool,
    pub range: Range,
}

#[derive(Debug, Clone, Default)]
pub struct FileIndex {
    pub uri: String,
    pub symbols: Vec<GlobalSymbol>,
    pub references: Vec<GlobalReference>,
    /// What the file's DROP statements remove (`*`: everything of the kind).
    pub drops: Vec<(SymbolKind, String)>,
}

impl FileIndex {
    /// Indexes a document. Statements broken by a misspelled keyword are
    /// indexed as corrected, so their declarations do not vanish meanwhile.
    /// With `workspace`, aliases in corrected statements are typed by its edges.
    pub fn of_document(
        uri: &str,
        tree: &Tree,
        analysis: &Analysis,
        source: &SourceText,
        encoding: PositionEncoding,
        workspace: Option<&Workspace>,
    ) -> FileIndex {
        if tree.root_node().has_error() {
            let (_, repair) = autocorrect::typos_and_repair(
                tree.root_node(),
                source,
                encoding,
            );
            if let Some(repair) = repair {
                let analysis = match workspace {
                    Some(workspace) => repair.analysis(workspace),
                    None => repair.analysis.clone(),
                };
                let mut index = FileIndex::build(
                    uri,
                    &analysis,
                    &repair.source,
                    encoding,
                );
                for symbol in &mut index.symbols {
                    symbol.range = repair.range(symbol.range);
                    symbol.selection = repair.range(symbol.selection);
                }
                for reference in &mut index.references {
                    reference.range = repair.range(reference.range);
                }
                return index;
            }
        }
        FileIndex::build(uri, analysis, source, encoding)
    }

    pub fn build(
        uri: &str,
        analysis: &Analysis,
        source: &SourceText,
        encoding: PositionEncoding,
    ) -> FileIndex {
        let symbols = analysis
            .symbols
            .iter()
            .filter(|s| s.is_exported())
            .map(|s| GlobalSymbol {
                uri: uri.to_string(),
                name: s.name.clone(),
                kind: s.kind,
                owner: s.owner.clone(),
                range: source.range(s.span, encoding),
                selection: source.range(s.name_span, encoding),
                detail: s.detail.clone(),
                doc: s.doc.clone(),
                ty: s.ty.clone(),
                graph: s.graph.clone(),
                params: s.params.clone(),
                returns: s.returns.clone(),
                members: s.members.clone(),
                ends: s.ends.clone(),
                vector: s.vector,
                id_only: s.id_only,
                strict_create: s.strict_create,
                altered: s.altered,
                in_error: s.in_error,
            })
            .collect();
        let references = analysis
            .references
            .iter()
            .filter(|r| !r.declaration)
            .filter_map(|r| {
                let (kinds, owners) = match analysis.target_symbol(r) {
                    Some(symbol)
                        if symbol.scope == FILE_SCOPE
                            || symbol.is_job_file() =>
                    {
                        (
                            vec![symbol.kind],
                            symbol.owner.iter().cloned().collect(),
                        )
                    }
                    Some(_) => return None,
                    None => global_candidates(&r.role)?,
                };
                Some(GlobalReference {
                    name: r.name.clone(),
                    kinds,
                    owners,
                    call: r.role == Role::Function,
                    range: source.range(r.span, encoding),
                })
            })
            .collect();
        FileIndex {
            uri: uri.to_string(),
            symbols,
            references,
            drops: analysis.drops.clone(),
        }
    }
}

/// The kinds of workspace declaration an unresolved occurrence can refer to.
pub fn global_candidates(
    role: &Role,
) -> Option<(Vec<SymbolKind>, Vec<String>)> {
    use SymbolKind as K;
    if let Some(kinds) = role.global_kinds() {
        return Some((kinds.to_vec(), Vec::new()));
    }
    match role {
        Role::TupleField(tuple) | Role::Attribute(Ty::Tuple(tuple)) => {
            Some((vec![K::TupleField], vec![tuple.clone()]))
        }
        Role::JobFile(job) => {
            Some((vec![K::FilenameVariable], vec![job.clone()]))
        }
        Role::Attribute(ty) => {
            Some((vec![K::Attribute], ty.attribute_owners()?.to_vec()))
        }
        _ => None,
    }
}

/// Whether two file versions (`None`: no file) declare the same edge types in order.
fn same_edge_types(old: Option<&FileIndex>, new: Option<&FileIndex>) -> bool {
    let (old, new) = (edge_types(old), edge_types(new));
    old.len() == new.len()
        && old.iter().zip(new).all(|(a, b)| {
            let (x, y) = (&a.ends, &b.ends);
            (&a.uri, &a.name, &x.from, &x.to, x.directed)
                == (&b.uri, &b.name, &y.from, &y.to, y.directed)
        })
}

/// The edge types a file declares, in file order.
fn edge_types(file: Option<&FileIndex>) -> Vec<&GlobalSymbol> {
    let mut edges: Vec<&GlobalSymbol> = file
        .into_iter()
        .flat_map(|f| &f.symbols)
        .filter(|s| s.kind == SymbolKind::EdgeType)
        .collect();
    edges.sort_by_key(|s| s.selection.start);
    edges
}

#[derive(Debug, Default)]
pub struct Workspace {
    pub roots: Vec<PathBuf>,
    /// How many scans of workspace folders are still running.
    indexers: usize,
    files: HashMap<String, FileIndex>,
    /// Symbol name -> (file key, index into that file's symbols).
    names: HashMap<String, Vec<(String, usize)>>,
    edge_generation: u64,
}

impl Workspace {
    /// Changes whenever the edge types do, so an analysis made with the workspace holds
    /// while this does. Values are unique across workspaces, except 0 for those that
    /// never had an edge type.
    pub fn edge_generation(&self) -> u64 {
        self.edge_generation
    }

    /// Starts a new edge generation if `new` changes the edge types at `key`.
    fn note_edges(&mut self, key: &str, new: Option<&FileIndex>) {
        static LAST: AtomicU64 = AtomicU64::new(0);
        if !same_edge_types(self.files.get(key), new) {
            self.edge_generation = LAST.fetch_add(1, Ordering::Relaxed) + 1;
        }
    }

    pub fn update(&mut self, index: FileIndex) {
        let key = uri::key(&index.uri);
        self.note_edges(&key, Some(&index));
        self.unindex(&key);
        for (position, symbol) in index.symbols.iter().enumerate() {
            self.names
                .entry(symbol.name.clone())
                .or_default()
                .push((key.clone(), position));
        }
        self.files.insert(key, index);
    }

    pub fn remove(&mut self, uri: &str) {
        let key = uri::key(uri);
        self.note_edges(&key, None);
        self.unindex(&key);
        self.files.remove(&key);
    }

    fn unindex(&mut self, key: &str) {
        let Some(old) = self.files.get(key) else {
            return;
        };
        for symbol in &old.symbols {
            if let Some(entries) = self.names.get_mut(&symbol.name) {
                entries.retain(|(file, _)| file != key);
                if entries.is_empty() {
                    self.names.remove(&symbol.name);
                }
            }
        }
    }

    /// Workspace-level declarations with this name, in any file.
    fn named(&self, name: &str) -> impl Iterator<Item = &GlobalSymbol> {
        self.names
            .get(name)
            .into_iter()
            .flatten()
            .filter_map(|(file, position)| {
                self.files
                    .get(file)
                    .and_then(|f| f.symbols.get(*position))
            })
    }

    /// Whether a DROP statement in any file removes the type, graph or query.
    pub fn is_dropped(&self, kind: SymbolKind, name: &str) -> bool {
        self.files.values().any(|f| {
            f.drops
                .iter()
                .any(|(k, n)| *k == kind && (n == "*" || n == name))
        })
    }

    pub fn start_indexing(&mut self) {
        self.indexers += 1;
    }

    pub fn finish_indexing(&mut self) {
        self.indexers = self.indexers.saturating_sub(1);
    }

    /// A scan of the workspace folders is still running, so the schema may not be
    /// known yet.
    pub fn indexing(&self) -> bool {
        self.indexers > 0
    }

    /// Whether `path` lies under one of the workspace roots.
    pub fn contains(&self, path: &Path) -> bool {
        let canonical = |p: &Path| {
            std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
        };
        let path = canonical(path);
        self.roots
            .iter()
            .any(|root| path.starts_with(canonical(root)))
    }

    pub fn file(&self, uri: &str) -> Option<&FileIndex> {
        self.files.get(&uri::key(uri))
    }

    pub fn files(&self) -> impl Iterator<Item = &FileIndex> {
        self.files.values()
    }

    pub fn symbols(&self) -> impl Iterator<Item = &GlobalSymbol> {
        self.files
            .values()
            .flat_map(|f| f.symbols.iter())
    }

    pub fn find(&self, kind: SymbolKind, name: &str) -> Vec<&GlobalSymbol> {
        self.find_any(&[kind], name)
    }

    /// (In a fixed order, by file and position: the files are indexed in any order.)
    pub fn find_any(
        &self,
        kinds: &[SymbolKind],
        name: &str,
    ) -> Vec<&GlobalSymbol> {
        let mut found: Vec<_> = self
            .named(name)
            .filter(|s| kinds.contains(&s.kind))
            .collect();
        found.sort_by(|a, b| {
            (&a.uri, a.selection.start).cmp(&(&b.uri, b.selection.start))
        });
        found
    }

    /// The declaration of `name` among `kinds`, if the workspace has exactly one.
    pub fn find_unique(
        &self,
        kinds: &[SymbolKind],
        name: &str,
    ) -> Option<&GlobalSymbol> {
        let mut found = self
            .named(name)
            .filter(|s| kinds.contains(&s.kind));
        let first = found.next()?;
        found.next().is_none().then_some(first)
    }

    /// Whether some workspace file declares `name` as one of `kinds`.
    pub fn declares(&self, kinds: &[SymbolKind], name: &str) -> bool {
        self.named(name)
            .any(|s| kinds.contains(&s.kind))
    }

    pub fn of_kind(&self, kind: SymbolKind) -> Vec<&GlobalSymbol> {
        let mut symbols: Vec<_> = self
            .symbols()
            .filter(|s| s.kind == kind)
            .collect();
        symbols.sort_by(|a, b| {
            (&a.name, &a.uri, a.selection.start).cmp(&(
                &b.name,
                &b.uri,
                b.selection.start,
            ))
        });
        symbols.dedup_by(|a, b| a.name == b.name && a.owner == b.owner);
        symbols
    }

    /// Attributes of `owner`, or of every vertex and edge type when `owner` is `None`.
    pub fn attributes(&self, owner: Option<&str>) -> Vec<&GlobalSymbol> {
        let mut attributes: Vec<_> = self
            .symbols()
            .filter(|s| {
                s.kind == SymbolKind::Attribute
                    && (owner.is_none() || s.owner.as_deref() == owner)
            })
            .collect();
        attributes.sort_by(|a, b| {
            (&a.owner, &a.name, &a.uri, a.selection.start).cmp(&(
                &b.owner,
                &b.name,
                &b.uri,
                b.selection.start,
            ))
        });
        attributes
    }

    /// The attributes of a type in the order they are declared: those of the
    /// CREATE first (by file, then position), then those added by ALTER, so a
    /// schema change job in a file that sorts first does not come before the schema.
    pub fn attributes_declared(&self, owner: &str) -> Vec<&GlobalSymbol> {
        let mut attributes: Vec<_> = self
            .symbols()
            .filter(|s| {
                s.kind == SymbolKind::Attribute
                    && s.owner.as_deref() == Some(owner)
            })
            .collect();
        attributes.sort_by(|a, b| {
            (a.altered, &a.uri, a.selection.start).cmp(&(
                b.altered,
                &b.uri,
                b.selection.start,
            ))
        });
        attributes
    }

    pub fn tuple_fields(&self, tuple: &str) -> Vec<&GlobalSymbol> {
        self.symbols()
            .filter(|s| {
                s.kind == SymbolKind::TupleField
                    && s.owner.as_deref() == Some(tuple)
            })
            .collect()
    }

    /// Whether any vertex type is declared, i.e. whether schema checks are meaningful.
    pub fn has_schema(&self) -> bool {
        self.symbols()
            .any(|s| s.kind == SymbolKind::VertexType)
    }

    /// Locations of every reference to a workspace-level declaration.
    /// Attribute references whose owner could not be inferred are only
    /// included when `include_uncertain` is set.
    pub fn references(
        &self,
        kind: SymbolKind,
        name: &str,
        owner: Option<&str>,
        include_uncertain: bool,
    ) -> Vec<Location> {
        let mut locations = Vec::new();
        for file in self.files.values() {
            for reference in &file.references {
                if reference.name != name || !reference.kinds.contains(&kind)
                {
                    continue;
                }
                let owner_matches = match owner {
                    None => true,
                    Some(owner) if reference.owners.is_empty() => {
                        include_uncertain && !owner.is_empty()
                    }
                    Some(owner) => {
                        reference.owners.iter().any(|o| o == owner)
                    }
                };
                if owner_matches {
                    locations.push(Location {
                        uri: file.uri.clone(),
                        range: reference.range,
                    });
                }
            }
        }
        locations
    }
}

/// The GSQL files directly inside `dir` (no subfolders), for a file that is
/// outside every workspace folder: its neighbours may hold the schema.
pub fn scan_directory(dir: &Path) -> Vec<PathBuf> {
    const MAX_NEIGHBOURS: usize = 200;
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut files: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| {
            is_gsql_file(p)
                && std::fs::metadata(p)
                    .is_ok_and(|m| m.is_file() && m.len() <= MAX_FILE_BYTES)
        })
        .collect();
    files.sort();
    files.truncate(MAX_NEIGHBOURS);
    files
}

/// The nearest folder at or above the one holding `file` that has a `.gsqlroot`
/// marker: the project root for a file outside every workspace folder.
pub fn marked_root(file: &Path) -> Option<PathBuf> {
    // Without `..` segments: the ancestors of `a/../b/q.gsql` are not those of the file.
    let file = uri::resolve_dots(file);
    file.parent()?
        .ancestors()
        .find(|dir| dir.join(ROOT_MARKER).is_file())
        .map(Path::to_path_buf)
}

/// The folder searched for the schema of a file outside every workspace
/// folder: the `.gsqlroot` folder, else the file's own folder.
pub fn loose_dir(file: &Path) -> Option<PathBuf> {
    let file = uri::resolve_dots(file);
    marked_root(&file).or_else(|| file.parent().map(Path::to_path_buf))
}

/// The `loose_dir` of a file, and the GSQL files to index from it: all under
/// a marked root, else the neighbours.
pub fn loose_project(file: &Path) -> Option<(PathBuf, Vec<PathBuf>)> {
    let dir = loose_dir(file)?;
    let files = if dir.join(ROOT_MARKER).is_file() {
        scan(std::slice::from_ref(&dir))
    } else {
        scan_directory(&dir)
    };
    Some((dir, files))
}

/// What `scan` found, and what it had to leave out.
#[derive(Debug, Default)]
pub struct ScanReport {
    /// The files to index, sorted by path.
    pub files: Vec<PathBuf>,
    /// GSQL files left out because of `MAX_FILES`.
    pub dropped_files: usize,
    /// Directories left unread because of `MAX_DIRECTORIES`.
    pub dropped_directories: usize,
    /// Directories left unread because 200,000 paths were already collected.
    pub unread_after_path_cap: usize,
    /// Files left out because they are larger than `MAX_FILE_BYTES`.
    pub oversize_files: usize,
}

impl ScanReport {
    /// The warning for an incomplete scan, naming each limit that was hit.
    pub fn warning(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.dropped_files > 0 {
            parts.push(format!(
                "only {MAX_FILES} GSQL files were indexed ({} more skipped; files that declare schema objects and files nearest the root come first)",
                self.dropped_files
            ));
        }
        if self.dropped_directories > 0 {
            parts.push(format!(
                "scanning stopped after {MAX_DIRECTORIES} folders ({} not read)",
                self.dropped_directories
            ));
        }
        if self.unread_after_path_cap > 0 {
            let n = self.unread_after_path_cap;
            parts.push(format!(
                "scanning stopped after collecting 200,000 paths ({n} folder{} not read)",
                plural(n)
            ));
        }
        if self.oversize_files > 0 {
            let n = self.oversize_files;
            parts.push(format!(
                "{n} file{} larger than {} MB {} skipped",
                plural(n),
                MAX_FILE_BYTES / (1024 * 1024),
                if n == 1 { "was" } else { "were" }
            ));
        }
        if parts.is_empty() {
            return None;
        }
        Some(format!(
            "gsql-lsp: the workspace index is incomplete: {}. Open a smaller folder to index everything.",
            parts.join("; ")
        ))
    }
}

/// Whether the start of a file declares schema objects (CREATE VERTEX, EDGE or GRAPH).
fn declares_schema(path: &Path) -> bool {
    use std::io::Read;
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = Vec::new();
    if file
        .take(64 * 1024)
        .read_to_end(&mut head)
        .is_err()
    {
        return false;
    }
    let text = String::from_utf8_lossy(&head).to_ascii_uppercase();
    text.match_indices("CREATE").any(|(i, _)| {
        let mut words = text[i + 6..].split_whitespace();
        let mut word = words.next();
        if matches!(word, Some("DIRECTED" | "UNDIRECTED")) {
            word = words.next();
        }
        matches!(word, Some("VERTEX" | "EDGE" | "GRAPH"))
    })
}

/// Every GSQL file under the given roots (hidden and build directories are
/// skipped). Symbolic links to directories are followed, once per real
/// directory: a link to a folder that is scanned anyway (a loop, a link to a
/// parent) adds nothing, and the real folder's spelling of a path wins.
pub fn scan(roots: &[PathBuf]) -> Vec<PathBuf> {
    scan_report(roots).files
}

/// Keeps the first of the paths that name the same file (by URI key).
pub fn dedupe_paths(files: &mut Vec<PathBuf>) {
    let mut seen = std::collections::HashSet::new();
    files.retain(|f| {
        let absolute = std::path::absolute(f).unwrap_or_else(|_| f.clone());
        seen.insert(uri::key(&uri::from_path(&absolute)))
    });
}

/// `scan`, with a report of the limits that were hit. Past `MAX_FILES` the
/// files that declare schema objects, then those nearest the root, are kept.
pub fn scan_report(roots: &[PathBuf]) -> ScanReport {
    scan_with_limit(roots, MAX_FILES)
}

fn scan_with_limit(roots: &[PathBuf], max_files: usize) -> ScanReport {
    /// Deepest folder level followed below a root.
    const MAX_DEPTH: usize = 100;
    /// Paths collected before the walk stops, so that a huge tree cannot exhaust memory.
    const MAX_COLLECTED: usize = 200_000;
    let canonical = |p: &Path| {
        std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    };
    let mut report = ScanReport::default();
    // Each file with its folder depth below the root.
    let mut found: Vec<(usize, PathBuf)> = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = roots
        .iter()
        .map(|r| (r.clone(), 0))
        .collect();
    // Links to directories, scanned after the real ones.
    let mut linked: Vec<(PathBuf, usize)> = Vec::new();
    let root_paths: Vec<PathBuf> =
        roots.iter().map(|r| canonical(r)).collect();
    let mut seen: std::collections::HashSet<PathBuf> =
        root_paths.iter().cloned().collect();
    let mut visited = 0;
    'walk: while let Some((dir, depth)) = stack.pop().or_else(|| {
        // Only now, with every real folder known, which links lead somewhere new.
        while let Some((link, depth)) = linked.pop() {
            let target = canonical(&link);
            // A link up to a folder that holds a root would take in the neighbours of the project.
            if root_paths
                .iter()
                .any(|root| root.starts_with(&target))
            {
                continue;
            }
            if seen.insert(target) {
                return Some((link, depth));
            }
        }
        None
    }) {
        visited += 1;
        if visited > MAX_DIRECTORIES {
            report.dropped_directories = 1 + stack.len() + linked.len();
            break;
        }
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let name = entry
                .file_name()
                .to_string_lossy()
                .into_owned();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            let linked_dir = file_type.is_symlink()
                && std::fs::metadata(&path).is_ok_and(|m| m.is_dir());
            if file_type.is_dir() || linked_dir {
                if name.starts_with('.')
                    || SKIPPED_DIRS.contains(&name.as_str())
                    || depth >= MAX_DEPTH
                {
                    continue;
                }
                if linked_dir {
                    linked.push((path, depth + 1));
                } else if seen.insert(canonical(&path)) {
                    stack.push((path, depth + 1));
                }
            // Symbolic links to files count.
            } else if (file_type.is_file() || file_type.is_symlink())
                && is_gsql_file(&path)
            {
                match std::fs::metadata(&path) {
                    Ok(m) if m.is_file() && m.len() <= MAX_FILE_BYTES => {
                        found.push((depth, path));
                        if found.len() >= MAX_COLLECTED {
                            report.unread_after_path_cap =
                                stack.len() + linked.len();
                            break 'walk;
                        }
                    }
                    Ok(m) if m.is_file() => report.oversize_files += 1,
                    _ => {}
                }
            }
        }
    }
    if found.len() > max_files {
        // Which files survive must not depend on the order of the directory listing.
        let mut ranked: Vec<(bool, usize, PathBuf)> = found
            .into_iter()
            .map(|(depth, path)| (!declares_schema(&path), depth, path))
            .collect();
        ranked.sort();
        report.dropped_files = ranked.len() - max_files;
        ranked.truncate(max_files);
        found = ranked
            .into_iter()
            .map(|(_, depth, path)| (depth, path))
            .collect();
    }
    report.files = found
        .into_iter()
        .map(|(_, path)| path)
        .collect();
    report.files.sort();
    report
}

pub fn is_gsql_file(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| {
            EXTENSIONS
                .iter()
                .any(|x| x.eq_ignore_ascii_case(e))
        })
}

/// The text of a file, with bytes that are not UTF-8 (a Latin-1 letter in a
/// comment) replaced by U+FFFD, so that such a file is still indexed.
pub fn read_text(path: &Path) -> std::io::Result<String> {
    let bytes = std::fs::read(path)?;
    Ok(String::from_utf8(bytes).unwrap_or_else(|err| {
        String::from_utf8_lossy(err.as_bytes()).into_owned()
    }))
}

/// Parses and indexes a file from disk.
pub fn index_file(
    path: &Path,
    encoding: PositionEncoding,
) -> Option<FileIndex> {
    let text = read_text(path).ok()?;
    let (tree, analysis) = Analysis::parse(&text);
    let source = SourceText::new(text);
    Some(FileIndex::of_document(
        &uri::from_path(path),
        &tree,
        &analysis,
        &source,
        encoding,
        None,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::test_support::{
        TYPO_BEFORE_AN_EDGE, acted_in, cursor,
    };

    fn index(uri: &str, text: &str) -> FileIndex {
        let (_, analysis) = Analysis::parse(text);
        FileIndex::build(
            uri,
            &analysis,
            &SourceText::new(text.to_string()),
            PositionEncoding::Utf16,
        )
    }

    #[test]
    fn indexing_lasts_until_every_scan_has_finished() {
        let mut workspace = Workspace::default();
        assert!(!workspace.indexing());
        workspace.start_indexing();
        workspace.start_indexing();
        workspace.finish_indexing();
        assert!(workspace.indexing(), "one scan is still running");
        workspace.finish_indexing();
        assert!(!workspace.indexing());
    }

    #[test]
    fn reads_text_that_is_not_utf8_lossily() {
        let path = std::env::temp_dir()
            .join(format!("gsql-latin1-{}.gsql", std::process::id()));
        std::fs::write(
            &path,
            b"// caf\xe9\nCREATE VERTEX P (PRIMARY_ID id STRING)\n",
        )
        .unwrap();
        let text = read_text(&path).unwrap();
        let indexed = index_file(&path, PositionEncoding::Utf16).is_some();
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            text,
            "// caf\u{fffd}\nCREATE VERTEX P (PRIMARY_ID id STRING)\n"
        );
        assert!(indexed);
    }

    #[test]
    fn scan_past_the_file_limit_keeps_schema_files_and_reports() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a/b/c")).unwrap();
        for i in 0..4 {
            std::fs::write(
                dir.join(format!("q{i}.gsql")),
                "CREATE QUERY q() { }\n",
            )
            .unwrap();
        }
        std::fs::write(
            dir.join("a/b/c/zz.gsql"),
            "-- x\ncreate  undirected edge E (FROM A, TO B)\n",
        )
        .unwrap();
        let report = scan_with_limit(std::slice::from_ref(&dir), 3);
        let names: Vec<String> = report
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&dir)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/")
            })
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(names, ["a/b/c/zz.gsql", "q0.gsql", "q1.gsql"]);
        assert_eq!(report.dropped_files, 2);
        assert!(
            report
                .warning()
                .unwrap()
                .contains("incomplete")
        );
    }

    #[test]
    fn scan_within_the_limits_has_no_warning() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-nolimit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("q.gsql"), "").unwrap();
        let report = scan_report(std::slice::from_ref(&dir));
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(report.files.len(), 1);
        assert!(report.warning().is_none());
    }

    #[test]
    fn scan_warning_counts_in_singular_and_plural() {
        let warning = |count: usize| {
            ScanReport {
                unread_after_path_cap: count,
                oversize_files: count,
                ..ScanReport::default()
            }
            .warning()
            .unwrap()
        };
        let one = warning(1);
        assert!(one.contains("(1 folder not read)"), "{one}");
        assert!(one.contains("1 file larger than 8 MB was skipped"), "{one}");
        let two = warning(2);
        assert!(two.contains("(2 folders not read)"), "{two}");
        assert!(
            two.contains("2 files larger than 8 MB were skipped"),
            "{two}"
        );
    }

    #[test]
    fn attributes_added_by_alter_come_after_those_of_the_create() {
        let mut workspace = Workspace::default();
        workspace.update(index(
            "file:///jobs/a.gsql",
            "CREATE SCHEMA_CHANGE JOB j { ALTER VERTEX P ADD ATTRIBUTE (email STRING); }\n",
        ));
        workspace.update(index(
            "file:///schema/s.gsql",
            "CREATE VERTEX P (PRIMARY_ID id STRING, name STRING)\n",
        ));
        let names: Vec<&str> = workspace
            .attributes_declared("P")
            .iter()
            .map(|a| a.name.as_str())
            .collect();
        assert_eq!(names, ["id", "name", "email"]);
    }

    #[test]
    fn marked_root_is_the_nearest_folder_with_the_marker() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-marker-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("p/queries")).unwrap();
        std::fs::create_dir_all(dir.join("p/schema")).unwrap();
        std::fs::write(dir.join("p/schema/s.gsql"), "").unwrap();
        assert_eq!(marked_root(&dir.join("p/queries/q.gsql")), None);
        std::fs::write(dir.join("p/.gsqlroot"), "").unwrap();
        assert_eq!(
            marked_root(&dir.join("p/queries/q.gsql")),
            Some(dir.join("p"))
        );
        // A nearer marker wins.
        std::fs::write(dir.join("p/queries/.gsqlroot"), "").unwrap();
        assert_eq!(
            marked_root(&dir.join("p/queries/q.gsql")),
            Some(dir.join("p/queries"))
        );
        let (root, files) =
            loose_project(&dir.join("p/schema/s.gsql")).unwrap();
        assert_eq!(
            (root, files),
            (dir.join("p"), vec![dir.join("p/schema/s.gsql")])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn scan_directory_reads_one_folder_only() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-shallow-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("a.gsql"), "").unwrap();
        std::fs::write(dir.join("notes.txt"), "").unwrap();
        std::fs::write(dir.join("sub/b.gsql"), "").unwrap();
        let found: Vec<String> = scan_directory(&dir)
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found, ["a.gsql"]);
    }

    #[cfg(unix)]
    #[test]
    fn scan_finds_linked_files_and_skips_dangling_links() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-scan-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        std::fs::write(dir.join("real/schema.gsql"), "").unwrap();
        std::os::unix::fs::symlink(
            dir.join("real/schema.gsql"),
            dir.join("linked.gsql"),
        )
        .unwrap();
        std::os::unix::fs::symlink(
            dir.join("missing.gsql"),
            dir.join("dangling.gsql"),
        )
        .unwrap();
        let found: Vec<String> = scan(std::slice::from_ref(&dir))
            .iter()
            .map(|p| {
                p.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect();
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(found, ["linked.gsql", "schema.gsql"]);
    }

    #[cfg(unix)]
    #[test]
    fn scan_follows_linked_directories_once() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-dirlink-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("shared/schema")).unwrap();
        std::fs::create_dir_all(dir.join("proj/sub")).unwrap();
        std::fs::write(dir.join("shared/schema/s.gsql"), "").unwrap();
        std::fs::write(dir.join("proj/q.gsql"), "").unwrap();
        let link = std::os::unix::fs::symlink;
        link(dir.join("shared/schema"), dir.join("proj/schema")).unwrap();
        // A loop, a link to the parent and a dangling link add nothing and do not hang.
        link(dir.join("proj"), dir.join("proj/sub/loop")).unwrap();
        link(dir.join("proj"), dir.join("proj/sub/up")).unwrap();
        link(dir.join("nowhere"), dir.join("proj/gone")).unwrap();
        let names = |files: Vec<PathBuf>| -> Vec<String> {
            files
                .iter()
                .map(|p| {
                    p.strip_prefix(&dir)
                        .unwrap()
                        .display()
                        .to_string()
                })
                .collect()
        };
        assert_eq!(
            names(scan(&[dir.join("proj")])),
            ["proj/q.gsql", "proj/schema/s.gsql"]
        );
        // The real folder's spelling wins when both are in the scan.
        assert_eq!(
            names(scan(std::slice::from_ref(&dir))),
            ["proj/q.gsql", "shared/schema/s.gsql"]
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn dot_dot_segments_do_not_change_the_project() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-dotdot-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("a")).unwrap();
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("a/.gsqlroot"), "").unwrap();
        std::fs::write(dir.join("b/q.gsql"), "").unwrap();
        let tricky = dir.join("a/../b/q.gsql");
        assert_eq!(marked_root(&tricky), None);
        let (folder, files) = loose_project(&tricky).unwrap();
        let base = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(
            (folder, files),
            (base.join("b"), vec![base.join("b/q.gsql")])
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn a_link_up_to_a_folder_holding_the_root_adds_no_neighbours() {
        let dir = std::env::temp_dir()
            .join(format!("gsql-scan-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("p/q")).unwrap();
        std::fs::create_dir_all(dir.join("other")).unwrap();
        std::fs::write(dir.join("p/mine.gsql"), "").unwrap();
        std::fs::write(dir.join("other/theirs.gsql"), "").unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../..", dir.join("p/q/up")).unwrap();
            let names: Vec<String> = scan(&[dir.join("p")])
                .iter()
                .map(|f| {
                    f.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .into_owned()
                })
                .collect();
            assert_eq!(names, ["mine.gsql"], "{names:?}");
        }
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn attribute_candidates_follow_the_type_of_the_object() {
        use SymbolKind as K;
        let names =
            |types: &[&str]| types.iter().map(|t| t.to_string()).collect();
        let candidates = |ty: Ty| global_candidates(&Role::Attribute(ty));
        assert_eq!(
            candidates(Ty::Unknown),
            Some((vec![K::Attribute], vec![]))
        );
        assert_eq!(
            candidates(Ty::VertexSet(names(&["Person", "Company"]))),
            Some((vec![K::Attribute], names(&["Person", "Company"])))
        );
        assert_eq!(
            candidates(Ty::Edge(names(&["follows"]))),
            Some((vec![K::Attribute], names(&["follows"])))
        );
        assert_eq!(
            candidates(Ty::Tuple("Pair".into())),
            Some((vec![K::TupleField], names(&["Pair"])))
        );
        assert_eq!(candidates(Ty::Primitive("STRING".into())), None);
        assert_eq!(
            candidates(Ty::Collection("LIST".into(), vec![Ty::Unknown])),
            None
        );
    }

    #[test]
    fn name_index_follows_updates_and_removals() {
        let mut workspace = Workspace::default();
        workspace.update(index(
            "file:///w/a.gsql",
            "CREATE VERTEX Person (PRIMARY_ID id STRING)\n",
        ));
        assert_eq!(
            workspace
                .find(SymbolKind::VertexType, "Person")
                .len(),
            1
        );
        workspace.update(index(
            "file:///w/a.gsql",
            "CREATE VERTEX Human (PRIMARY_ID id STRING)\n",
        ));
        assert!(
            workspace
                .find(SymbolKind::VertexType, "Person")
                .is_empty()
        );
        assert_eq!(
            workspace
                .find(SymbolKind::VertexType, "Human")
                .len(),
            1
        );
        workspace.update(index(
            "file:///w/b.gsql",
            "CREATE VERTEX Human (PRIMARY_ID id STRING)\n",
        ));
        assert_eq!(
            workspace
                .find(SymbolKind::VertexType, "Human")
                .len(),
            2
        );
        workspace.remove("file:///w/a.gsql");
        let remaining = workspace.find(SymbolKind::VertexType, "Human");
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].uri, "file:///w/b.gsql");
        assert!(
            workspace
                .find_any(&[SymbolKind::Attribute], "id")
                .iter()
                .all(|s| s.uri == "file:///w/b.gsql")
        );
    }

    #[test]
    fn indexes_schema_and_references_across_files() {
        let mut workspace = Workspace::default();
        workspace.update(index(
            "file:///w/schema.gsql",
            "CREATE VERTEX Person (PRIMARY_ID id STRING, age INT)\nCREATE DIRECTED EDGE Knows (FROM Person, TO Person) WITH REVERSE_EDGE=\"rev_knows\"\n",
        ));
        workspace.update(index(
            "file:///w/q.gsql",
            "CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT t FROM S:s -(Knows>:e)- Person:t WHERE t.age > 3;\n}\n",
        ));
        assert!(workspace.has_schema());
        assert_eq!(
            workspace
                .find(SymbolKind::EdgeType, "rev_knows")
                .len(),
            1
        );
        assert_eq!(workspace.attributes(Some("Person")).len(), 2);
        let person_refs = workspace.references(
            SymbolKind::VertexType,
            "Person",
            None,
            false,
        );
        // Two in the edge definition, two in the query.
        assert_eq!(person_refs.len(), 4, "{person_refs:?}");
        let age_refs = workspace.references(
            SymbolKind::Attribute,
            "age",
            Some("Person"),
            false,
        );
        assert_eq!(age_refs.len(), 1);
        assert_eq!(age_refs[0].uri, "file:///w/q.gsql");
    }

    #[test]
    fn the_edge_generation_follows_the_edge_types_only() {
        let (a, b) = ("file:///w/a.gsql", "file:///w/b.gsql");
        let edge = |declaration: &str| {
            format!(
                "CREATE VERTEX P (PRIMARY_ID id STRING)\nCREATE {declaration}\n"
            )
        };
        let p_to_p = edge("DIRECTED EDGE E (FROM P, TO P)");
        let mut workspace = Workspace::default();
        workspace
            .update(index(a, "CREATE VERTEX P (PRIMARY_ID id STRING)\n"));
        assert_eq!(workspace.edge_generation(), 0, "no edge type yet");
        workspace.update(index(a, &p_to_p));
        let first = workspace.edge_generation();
        assert_ne!(first, 0);
        // Another file, and the same edges elsewhere in their file.
        workspace.update(index(b, "CREATE QUERY q() { PRINT 1; }\n"));
        workspace.update(index(a, &format!("\n\n{p_to_p}")));
        assert_eq!(workspace.edge_generation(), first);
        // Each of `TO`, `FROM`, direction and name, changed alone.
        let mut seen = vec![0, first];
        for changed in [
            "DIRECTED EDGE E (FROM P, TO Q)",
            "DIRECTED EDGE E (FROM Q, TO Q)",
            "UNDIRECTED EDGE E (FROM Q, TO Q)",
            "UNDIRECTED EDGE F (FROM Q, TO Q)",
        ] {
            workspace.update(index(a, &edge(changed)));
            let generation = workspace.edge_generation();
            assert!(!seen.contains(&generation), "{changed}");
            seen.push(generation);
        }
        workspace.remove(b);
        assert_eq!(Some(&workspace.edge_generation()), seen.last());
        workspace.remove(a);
        assert!(!seen.contains(&workspace.edge_generation()));
        seen.push(workspace.edge_generation());
        let mut other = Workspace::default();
        other.update(index(b, &p_to_p));
        assert!(!seen.contains(&other.edge_generation()));
    }

    #[test]
    fn a_corrected_document_is_indexed_with_the_edges_of_the_workspace() {
        // `m` is a Movie only through the schema's edge.
        let mut workspace = Workspace::default();
        workspace.update(index("file:///w/schema.gsql", &acted_in("Movie")));
        let (text, _) = cursor(TYPO_BEFORE_AN_EDGE);
        let (tree, analysis) = Analysis::parse(&text);
        let source = SourceText::new(text);
        let of_document = |workspace| {
            let uri = "file:///w/q.gsql";
            let encoding = PositionEncoding::Utf16;
            FileIndex::of_document(
                uri, &tree, &analysis, &source, encoding, workspace,
            )
        };
        let owners = |index: &FileIndex| {
            let name = index
                .references
                .iter()
                .find(|r| r.name == "name");
            name.expect("m.name").owners.clone()
        };
        assert!(owners(&of_document(None)).is_empty());
        let typed = of_document(Some(&workspace));
        assert_eq!(owners(&typed), ["Movie"]);
        workspace.update(typed);
        let found = workspace.references(
            SymbolKind::Attribute,
            "name",
            Some("Movie"),
            false,
        );
        assert!(
            found
                .iter()
                .any(|l| l.uri == "file:///w/q.gsql"),
            "{found:?}"
        );
    }
}
