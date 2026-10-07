//! A reference of the built-in functions, methods, accumulators, types and
//! keywords, as a file that "go to definition" can jump into. There is no
//! source to show for a built-in, so the server writes its documentation to a
//! file in the cache folder (comments only, so it has no diagnostics) and the
//! locations point at the entry. Every editor can open it without help.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use crate::builtin_docs;
use crate::builtins::{self, Method};
use crate::lsp::types::{Location, Position, Range};
use crate::uri;

struct Reference {
    text: String,
    /// Lookup key of an entry -> the line of its heading.
    lines: HashMap<String, u32>,
}

#[derive(Default)]
struct Writer {
    text: String,
    lines: HashMap<String, u32>,
    line: u32,
}

/// The text of a `/* ... */` comment, safe to put in one.
fn comment(doc: &str, indent: &str) -> String {
    let doc = doc.replace("*/", "* /");
    let mut lines = doc.lines();
    let mut out = format!("{indent}/* {}", lines.next().unwrap_or_default());
    for line in lines {
        out.push('\n');
        if !line.trim().is_empty() {
            out.push_str(&format!("{indent}   {line}"));
        }
    }
    out.push_str(" */");
    out
}

impl Writer {
    fn push(&mut self, text: &str) {
        for line in text.lines() {
            self.text.push_str(line);
            self.text.push('\n');
            self.line += 1;
        }
    }

    fn section(&mut self, title: &str) {
        self.push(&format!("\n// ===== {title} =====\n"));
    }

    /// A declaration followed by its documentation, as in a Python stub;
    /// definitions point at the declaration.
    fn entry(&mut self, keys: &[String], declaration: &str, doc: &str, indent: &str) {
        for key in keys {
            self.lines.entry(key.clone()).or_insert(self.line);
        }
        self.push(&format!("{indent}{declaration}"));
        self.push(&comment(doc, indent));
    }

    fn methods(&mut self, methods: &[Method]) {
        for method in methods {
            let signature = method.signature();
            // The exact entry of a method whose receiver is known, then by
            // text for the rest.
            let keys = [
                format!("method@{:p}", method),
                format!("method:{signature}|{}", method.doc),
                format!("method:{signature}"),
            ];
            let kind = if method.mutator { "MUTATOR" } else { "METHOD" };
            let declaration = format!("{kind} {};", signature.trim_start_matches('.'));
            let doc = builtin_docs::method(method).map_or_else(|| method.doc.to_string(), |page| page.plain());
            self.entry(&keys, &declaration, &doc, "  ");
        }
    }

    /// `BUILTIN OBJECT name syntax { doc; methods }`
    fn object(&mut self, keys: &[String], name: &str, syntax: &str, doc: &str, methods: &[Method]) {
        for key in keys {
            self.lines.entry(key.clone()).or_insert(self.line);
        }
        let syntax = syntax.strip_prefix(name).unwrap_or(syntax);
        self.push(&format!("BUILTIN OBJECT {name}{syntax} {{"));
        self.push(&comment(doc, "  "));
        self.methods(methods);
        self.push("}");
    }
}

fn build() -> Reference {
    let mut w = Writer::default();
    w.push(&format!("// GSQL built-ins, declared as stubs (gsql-lsp {}).", env!("CARGO_PKG_VERSION")));
    w.push("// Generated: changes to this file are lost. It is what \"go to definition\" opens for built-ins.");

    w.section("Functions");
    let mut category = None;
    for function in builtins::FUNCTIONS {
        if category != Some(function.category) {
            category = Some(function.category);
            w.push(&format!("\n// -- {} --", function.category.label()));
        }
        let keys = [format!("fn:{}", function.name.to_ascii_lowercase())];
        let doc = builtin_docs::function(function.name).map_or_else(|| function.doc.to_string(), |page| page.plain());
        w.entry(&keys, &format!("BUILTIN FUNCTION {};", function.signature()), &doc, "");
    }

    w.section("Methods");
    for (name, doc, methods) in [
        ("vertex", "A vertex, such as the alias of a SELECT block.", builtins::VERTEX_METHODS),
        ("edge", "An edge, such as the alias of an edge in a SELECT block.", builtins::EDGE_METHODS),
        ("vertex_set", "A vertex set variable.", builtins::VERTEX_SET_METHODS),
        ("JSONOBJECT", "A JSON object.", builtins::JSON_OBJECT_METHODS),
        ("JSONARRAY", "A JSON array.", builtins::JSON_ARRAY_METHODS),
        ("FILE", "An output file.", builtins::FILE_METHODS),
    ] {
        w.object(&[], name, "", doc, methods);
    }

    w.section("Accumulators");
    for accumulator in builtins::ACCUMULATORS {
        let keys = [format!("acc:{}", accumulator.name.to_ascii_lowercase())];
        let doc =
            builtin_docs::accumulator(accumulator.name).map_or_else(|| accumulator.doc.to_string(), |p| p.plain());
        w.object(&keys, accumulator.name, accumulator.syntax, &doc, accumulator.methods);
    }

    w.section("Types");
    for (name, doc) in builtins::PRIMITIVE_TYPES {
        w.entry(&[format!("type:{name}")], &format!("BUILTIN TYPE {name};"), doc, "");
    }

    w.section("Constants");
    for (name, doc) in builtins::CONSTANTS {
        w.entry(&[format!("const:{name}")], &format!("BUILTIN CONSTANT {name};"), doc, "");
    }

    w.section("Keywords");
    for (name, doc) in builtins::KEYWORDS {
        w.entry(&[format!("kw:{name}")], &format!("BUILTIN KEYWORD {name};"), doc, "");
    }
    Reference { text: w.text, lines: w.lines }
}

fn reference() -> &'static Reference {
    static REFERENCE: OnceLock<Reference> = OnceLock::new();
    REFERENCE.get_or_init(build)
}

fn file_name() -> String {
    format!("reference-{}.gsql", env!("CARGO_PKG_VERSION"))
}

/// The folders the reference file may be kept in, in order of preference: the
/// platform's cache folder, then a folder of the user in the temporary folder
/// (for a missing or read-only home).
fn candidate_dirs() -> Vec<PathBuf> {
    let var = |name: &str| std::env::var_os(name).map(PathBuf::from).filter(|p| p.is_absolute());
    // ~/Library/Caches, $XDG_CACHE_HOME or ~/.cache, %LOCALAPPDATA%.
    let base = if cfg!(windows) {
        var("LOCALAPPDATA")
    } else if cfg!(target_os = "macos") {
        var("HOME").map(|h| h.join("Library/Caches"))
    } else {
        var("XDG_CACHE_HOME").or_else(|| var("HOME").map(|h| h.join(".cache")))
    };
    let user = ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .filter_map(|name| std::env::var(name).ok())
        .map(|user| user.chars().filter(|c| c.is_ascii_alphanumeric() || "-_.".contains(*c)).collect::<String>())
        .find(|user| !user.is_empty() && user != "." && user != "..");
    let mut dirs: Vec<PathBuf> = base.into_iter().map(|b| b.join("gsql-lsp")).collect();
    dirs.push(std::env::temp_dir().join(match user {
        Some(user) => format!("gsql-lsp-{user}"),
        None => "gsql-lsp".to_string(),
    }));
    dirs
}

/// Creates `dir` (private to the user where the platform has such a thing).
fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir)?;
        // In the shared temp folder, a folder made by someone else beforehand (a
        // link, or open to others) is not trusted: only a private real folder is.
        if dir.starts_with(std::env::temp_dir()) {
            let meta = std::fs::symlink_metadata(dir)?;
            if !meta.is_dir() || meta.permissions().mode() & 0o077 != 0 {
                return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "folder not private"));
            }
        }
        Ok(())
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)
}

/// Writes the reference file into the first of `dirs` where that works (or
/// finds it there up to date).
fn write_to_first(dirs: &[PathBuf], text: &str) -> Option<PathBuf> {
    dirs.iter().find_map(|dir| {
        let path = dir.join(file_name());
        if std::fs::read_to_string(&path).is_ok_and(|existing| existing == text) {
            return Some(path);
        }
        create_private_dir(dir).ok()?;
        // Written beside and renamed, so an editor never reads half of it, and
        // servers started together do not write to the same file (the name
        // holds the process and a counter).
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let partial = path.with_extension(format!("tmp{}-{n}", std::process::id()));
        let written = std::fs::write(&partial, text).and_then(|()| std::fs::rename(&partial, &path));
        if written.is_err() {
            let _ = std::fs::remove_file(&partial);
            // Another server may have won the race with the same text.
            return std::fs::read_to_string(&path).is_ok_and(|existing| existing == text).then_some(path);
        }
        Some(path)
    })
}

/// The reference file, written when it is missing or out of date. `None` when
/// no folder can hold it: definitions then have no location (hovers still work).
fn ensure_file() -> Option<&'static PathBuf> {
    static FILE: OnceLock<Option<PathBuf>> = OnceLock::new();
    FILE.get_or_init(|| write_to_first(&candidate_dirs(), &reference().text)).as_ref()
}

fn locate(keys: &[String]) -> Option<Location> {
    let line = keys.iter().find_map(|key| reference().lines.get(key))?;
    let path = ensure_file()?;
    let position = Position { line: *line, character: 0 };
    Some(Location { uri: uri::from_path(path), range: Range { start: position, end: position } })
}

/// The text of the reference file.
#[cfg(test)]
pub fn text() -> &'static str {
    &reference().text
}

pub fn function(name: &str) -> Option<Location> {
    locate(&[format!("fn:{}", name.to_ascii_lowercase())])
}

pub fn method(method: &Method) -> Option<Location> {
    let signature = method.signature();
    locate(&[
        format!("method@{:p}", method),
        format!("method:{signature}|{}", method.doc),
        format!("method:{signature}"),
    ])
}

pub fn constant(name: &str) -> Option<Location> {
    locate(&[format!("const:{name}")])
}

pub fn accumulator(name: &str) -> Option<Location> {
    locate(&[format!("acc:{}", name.to_ascii_lowercase())])
}

/// A keyword or built-in type name (`FOREACH`, `DATETIME`).
pub fn keyword(name: &str) -> Option<Location> {
    let upper =
        name.split_whitespace().collect::<Vec<_>>().join(" ").to_ascii_uppercase().replace("POST_ACCUM", "POST-ACCUM");
    locate(&[format!("type:{upper}"), format!("kw:{upper}")])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line_of(key: &str) -> &'static str {
        let line = reference().lines[key] as usize;
        reference().text.lines().nth(line).unwrap()
    }

    #[test]
    fn falls_back_to_the_next_folder_and_gives_up_quietly() {
        let base = std::env::temp_dir().join(format!("gsql-refdir-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        std::fs::create_dir_all(&base).unwrap();
        // A regular file where a folder is needed: the first candidate fails.
        std::fs::write(base.join("blocked"), "").unwrap();
        let first = base.join("blocked/gsql-lsp");
        let second = base.join("tmp/gsql-lsp-me");
        let written = write_to_first(&[first.clone(), second.clone()], "text").unwrap();
        assert_eq!(written, second.join(file_name()));
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "text");
        // Again (up to date), and with new text.
        assert_eq!(write_to_first(&[first.clone(), second.clone()], "text").unwrap(), written);
        write_to_first(&[first.clone(), second.clone()], "newer").unwrap();
        assert_eq!(std::fs::read_to_string(&written).unwrap(), "newer");
        // Nothing works: no location.
        assert_eq!(write_to_first(&[first], "text"), None);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let locked = base.join("locked");
            std::fs::create_dir_all(&locked).unwrap();
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
            let result = write_to_first(&[locked.join("gsql-lsp")], "text");
            std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
            // (Root can write anywhere.)
            let root = std::process::Command::new("id").arg("-u").output().is_ok_and(|o| o.stdout == b"0\n");
            assert!(root || result.is_none());
        }
        std::fs::remove_dir_all(&base).unwrap();
    }

    #[test]
    fn candidates_end_with_a_temp_folder() {
        let dirs = candidate_dirs();
        assert!(dirs.last().unwrap().starts_with(std::env::temp_dir()));
        assert!(dirs.iter().all(|d| d.is_absolute()));
    }

    #[test]
    fn entries_start_at_their_declarations() {
        assert_eq!(line_of("fn:datetime_format"), "BUILTIN FUNCTION datetime_format(date, [str]) -> STRING;");
        assert!(line_of("acc:sumaccum").starts_with("BUILTIN OBJECT SumAccum<"));
        assert_eq!(line_of("type:DATETIME"), "BUILTIN TYPE DATETIME;");
        assert_eq!(line_of("kw:ACCUM"), "BUILTIN KEYWORD ACCUM;");
        assert_eq!(line_of("const:GSQL_INT_MAX"), "BUILTIN CONSTANT GSQL_INT_MAX;");
        assert_eq!(line_of("method:.size() -> INT"), "  METHOD size() -> INT;");
    }

    #[test]
    fn methods_of_each_accumulator_are_under_its_own_object() {
        let object_of = |target: &Method| {
            let line = method(target).unwrap().range.start.line as usize;
            let text = reference().text.lines().take(line).collect::<Vec<_>>();
            text.iter().rev().find(|l| l.starts_with("BUILTIN OBJECT ")).unwrap().to_string()
        };
        for accumulator in builtins::ACCUMULATORS {
            for m in accumulator.methods {
                let object = object_of(m);
                assert!(
                    object.starts_with(&format!("BUILTIN OBJECT {}", accumulator.name)),
                    "{}.{} is under {object}",
                    accumulator.name,
                    m.name
                );
            }
        }
        let or = builtins::accumulator("BitwiseOrAccum").unwrap();
        assert!(object_of(builtins::find_method(or.methods, "cardinality").unwrap()).contains("BitwiseOrAccum"));
        let and = builtins::accumulator("BitwiseAndAccum").unwrap();
        assert!(object_of(builtins::find_method(and.methods, "cardinality").unwrap()).contains("BitwiseAndAccum"));
    }

    #[test]
    fn every_function_has_an_entry() {
        for function in builtins::FUNCTIONS {
            assert!(reference().lines.contains_key(&format!("fn:{}", function.name.to_ascii_lowercase())));
        }
    }

    #[test]
    fn the_file_is_valid_stub_syntax() {
        let text = &reference().text;
        let tree = crate::syntax::parse(&mut crate::syntax::new_parser(), text, None);
        let mut errors = Vec::new();
        crate::syntax::walk_with_errors(tree.root_node(), |node, _| {
            if node.is_error() || node.is_missing() {
                let row = node.start_position().row;
                errors.push(format!("line {}: {}", row + 1, text.lines().nth(row).unwrap_or_default()));
            }
        });
        assert!(errors.is_empty(), "{:#?}", &errors[..errors.len().min(10)]);
    }
}
