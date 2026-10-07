//! `gsql-lsp check`: report diagnostics for files on the command line.

use std::path::{Path, PathBuf};

use crate::features::{Config, Snapshot};
use crate::lsp::types::severity;
use crate::text::{PositionEncoding, SourceText};
use crate::workspace::{self, FileIndex, Workspace};
use crate::{analysis, syntax, uri};

pub struct Options {
    pub paths: Vec<PathBuf>,
    /// Report only errors, not warnings and hints.
    pub errors_only: bool,
    pub format: OutputFormat,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OutputFormat {
    /// `path:line:column: severity: message [code]`
    #[default]
    Text,
    /// GitHub Actions workflow commands, shown as annotations on pull requests.
    Github,
    /// One JSON array of diagnostics.
    Json,
}

/// Escapes the message of a GitHub workflow command.
fn github_escape(text: &str) -> String {
    text.replace('%', "%25").replace('\r', "%0D").replace('\n', "%0A")
}

/// Escapes a property (such as `file=`) of a GitHub workflow command.
fn github_property(text: &str) -> String {
    github_escape(text).replace(':', "%3A").replace(',', "%2C")
}

/// What a run found, for the exit code.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Summary {
    /// Errors reported (`check`), or files not formatted (`format`).
    pub problems: usize,
    /// Requested files that could not be read or written.
    pub unreadable: usize,
}

/// Prints the diagnostics in `options.format`; read failures go to stderr.
pub fn run(options: &Options, out: &mut impl std::io::Write) -> std::io::Result<Summary> {
    let mut files = Vec::new();
    // Folders searched for the schema of explicit file arguments, as the
    // language server does for a file outside every workspace folder.
    let mut context = Vec::new();
    let mut searched = std::collections::HashSet::new();
    for path in &options.paths {
        if path.is_dir() {
            // Absolute, so that a file found through `.` and through its
            // `.gsqlroot` project has one URI (it is printed relative again).
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            files.extend(workspace::scan(std::slice::from_ref(&absolute)));
            // Below a `.gsqlroot`, the schema is searched in that project, as for
            // a file; only the files under the argument are reported.
            if let Some(root) = workspace::marked_root(&absolute.join("_"))
                && searched.insert(root.clone())
            {
                context.extend(workspace::scan(&[root]));
            }
        } else {
            // Absolute, so that the URI (and the walk up to a `.gsqlroot`) is right.
            let absolute = std::path::absolute(path).unwrap_or_else(|_| path.clone());
            files.push(absolute.clone());
            if let Some((dir, neighbours)) = workspace::loose_project(&absolute)
                && searched.insert(dir)
            {
                context.extend(neighbours);
            }
        }
    }
    // A file named twice (itself and through its folder) is checked once.
    let mut seen = std::collections::HashSet::new();
    files.retain(|file| seen.insert(uri::key(&uri::from_path(file))));
    let mut unreadable = 0;
    struct Parsed {
        path: PathBuf,
        source: SourceText,
        tree: tree_sitter::Tree,
        analysis: analysis::Analysis,
    }
    let mut parser = syntax::new_parser();
    let mut workspace = Workspace::default();
    let mut parsed = Vec::new();
    for path in files {
        let text = match workspace::read_text(&path) {
            Ok(text) => text,
            Err(err) => {
                eprintln!("{}: error: cannot read file: {err}", display(&path));
                unreadable += 1;
                continue;
            }
        };
        let tree = syntax::parse(&mut parser, &text, None);
        let analysis = analysis::analyze(&tree, &text);
        let source = SourceText::new(text);
        workspace.update(FileIndex::of_document(
            &uri::from_path(&path),
            &tree,
            &analysis,
            &source,
            PositionEncoding::Utf32,
        ));
        parsed.push(Parsed { path, source, tree, analysis });
    }
    let indexed: std::collections::HashSet<String> =
        parsed.iter().map(|f| uri::key(&uri::from_path(&f.path))).collect();
    let encoding = PositionEncoding::Utf32;
    for path in context {
        if !indexed.contains(&uri::key(&uri::from_path(&path)))
            && let Some(index) = workspace::index_file(&path, encoding)
        {
            workspace.update(index);
        }
    }
    // Aliases take vertex types from the schema's edges: analyze again.
    for file in parsed.iter_mut().filter(|f| f.analysis.uses_schema_edges) {
        file.analysis = analysis::analyze_in(&file.tree, &file.source.text, Some(&workspace));
    }
    let config = Config::default();
    let mut errors = 0;
    let mut json_items = Vec::new();
    for file in &parsed {
        let uri = uri::from_path(&file.path);
        let snapshot = Snapshot {
            uri: &uri,
            source: &file.source,
            tree: &file.tree,
            analysis: &file.analysis,
            workspace: &workspace,
            encoding: PositionEncoding::Utf32,
            config: &config,
        };
        // A bug in one file's analysis must not end the run for the others.
        let found = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            crate::features::diagnostics::diagnostics(&snapshot)
        }));
        let Ok(found) = found else {
            eprintln!("{}: error: internal error while checking this file (please report it)", display(&file.path));
            unreadable += 1;
            continue;
        };
        for diagnostic in found {
            let level = diagnostic.severity.unwrap_or(severity::ERROR);
            if level == severity::ERROR {
                errors += 1;
            } else if options.errors_only {
                continue;
            }
            let label = match level {
                severity::ERROR => "error",
                severity::WARNING => "warning",
                severity::INFORMATION => "info",
                _ => "hint",
            };
            let path = display(&file.path);
            let (line, column) = (diagnostic.range.start.line + 1, diagnostic.range.start.character + 1);
            let code = diagnostic.code.as_deref().unwrap_or("");
            match options.format {
                OutputFormat::Text => {
                    writeln!(out, "{path}:{line}:{column}: {label}: {} [{code}]", diagnostic.message)?;
                }
                OutputFormat::Github => {
                    let command = match level {
                        severity::ERROR => "error",
                        severity::WARNING => "warning",
                        _ => "notice",
                    };
                    writeln!(
                        out,
                        "::{command} file={},line={line},col={column},endLine={},endColumn={},title={}::{}",
                        github_property(&path),
                        diagnostic.range.end.line + 1,
                        diagnostic.range.end.character + 1,
                        github_property(&format!("gsql-lsp {code}")),
                        github_escape(&diagnostic.message),
                    )?;
                }
                OutputFormat::Json => json_items.push(serde_json::json!({
                    "path": path,
                    "line": line,
                    "column": column,
                    "endLine": diagnostic.range.end.line + 1,
                    "endColumn": diagnostic.range.end.character + 1,
                    "severity": label,
                    "code": code,
                    "message": diagnostic.message,
                })),
            }
        }
    }
    if options.format == OutputFormat::Json {
        writeln!(out, "{}", serde_json::Value::Array(json_items))?;
    }
    Ok(Summary { problems: errors, unreadable })
}

/// A path as printed in messages: relative to the current folder when inside it.
pub(crate) fn display(path: &Path) -> String {
    std::env::current_dir()
        .ok()
        .and_then(|cwd| path.strip_prefix(cwd).ok().map(|p| p.display().to_string()))
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_on(text: &str, format: OutputFormat) -> String {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let n = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("gsql-check-{}-{format:?}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("q.gsql");
        std::fs::write(&path, text).unwrap();
        let options = Options { paths: vec![path], errors_only: false, format };
        let mut out = Vec::new();
        run(&options, &mut out).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn non_reserved_statement_keywords_are_valid_variable_names() {
        let text = "CREATE QUERY q() FOR GRAPH G {\n  INT list = 1;\n  list = list + 1;\n  \
                    SetAccum<INT> file;\n  file.clear();\n  update = SELECT s FROM P:s;\n  \
                    insert = {P.*};\n  MAP<STRING, INT> map;\n  map = map;\n  PRINT list, map, file, update, insert;\n}\n";
        let out = run_on(text, OutputFormat::Text);
        assert!(!out.contains("syntax-error"), "{out}");
        // the statements those words start still parse as themselves
        let text = "CREATE QUERY q() FOR GRAPH G {\n  FILE f (\"x\");\n  UPDATE s FROM P:s SET s.a = 1;\n  \
                    INSERT INTO P (PRIMARY_ID, a) VALUES (\"x\", 1);\n}\n";
        let out = run_on(text, OutputFormat::Text);
        assert!(!out.contains("syntax-error"), "{out}");
    }

    #[test]
    fn non_reserved_keywords_are_valid_tuple_types_fields_and_job_names() {
        let text = "TYPEDEF TUPLE<list INT, map STRING, INT file, STRING update> list\n\
                    CREATE QUERY q() FOR GRAPH G {\n  ListAccum<list> @@l;\n  SetAccum<list> @@s;\n  \
                    list x = list(1, \"a\", 2, \"b\");\n  TUPLE<list INT, map STRING> t;\n  PRINT x.list, x.update;\n}\n\
                    CREATE LOADING JOB update FOR GRAPH G {\n  DEFINE FILENAME f;\n  LOAD f TO VERTEX P VALUES ($0, $1);\n}\n\
                    RUN LOADING JOB update\nSHOW LOADING STATUS list\nLIST WORKLOAD QUEUE\nDROP JOB update\n";
        let out = run_on(text, OutputFormat::Text);
        assert!(!out.contains("syntax-error"), "{out}");
    }

    #[test]
    fn command_keywords_are_valid_names_in_install_and_show_commands() {
        let text = "INSTALL QUERY clear\nSHOW QUERY get\nINSTALL QUERY use\nSHOW LOADING STATUS show\n\
                    SHOW JOB grant\nSHOW QUERY put\nSHOW VERTEX *\nABORT\nUSE GRAPH g\n";
        let out = run_on(text, OutputFormat::Text);
        assert!(!out.contains("syntax-error"), "{out}");
        assert!(!out.contains("did you mean"), "{out}");
    }

    #[test]
    fn braces_in_opencypher_strings_and_comments_do_not_end_the_body() {
        let text = "CREATE OR REPLACE OPENCYPHER QUERY oc(STRING s) FOR GRAPH G {\n  // a } here, don't\n  \
                    MATCH (p:Person {name: \"test}\"}) /* } */ WHERE p.name = '}' RETURN p.name\n}\n";
        let out = run_on(text, OutputFormat::Text);
        assert!(!out.contains("syntax-error"), "{out}");
        // a real unbalanced brace is still an error
        let out =
            run_on("CREATE OPENCYPHER QUERY oc() FOR GRAPH G {\n  MATCH (p {a: 1) RETURN p\n}\n", OutputFormat::Text);
        assert!(out.contains("rror"), "{out}");
    }

    #[test]
    fn writes_github_annotations_and_json() {
        let text = "CREATE QUERY q() {\n  @@total += 1;\n}\n";
        let github = run_on(text, OutputFormat::Github);
        assert!(github.starts_with("::error file="), "{github}");
        assert!(github.contains(",line=2,col=3,"), "{github}");
        assert!(github.contains("title=gsql-lsp undeclared-accumulator::The global accumulator"), "{github}");
        let json: serde_json::Value = serde_json::from_str(&run_on(text, OutputFormat::Json)).unwrap();
        assert_eq!(json[0]["line"], 2);
        assert_eq!(json[0]["severity"], "error");
        assert_eq!(json[0]["code"], "undeclared-accumulator");
    }

    const SCHEMA: &str = "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING)\nCREATE GRAPH G (Person)\n";
    const QUERY: &str =
        "CREATE QUERY q() FOR GRAPH G {\n  S = SELECT t FROM Person:t WHERE t.nme == \"a\";\n  PRINT S;\n}\n";

    fn project(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gsql-check-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("queries")).unwrap();
        std::fs::create_dir_all(dir.join("schema")).unwrap();
        std::fs::write(dir.join("schema/s.gsql"), SCHEMA).unwrap();
        std::fs::write(dir.join("queries/q.gsql"), QUERY).unwrap();
        dir
    }

    fn check(paths: Vec<PathBuf>) -> (String, Summary) {
        let options = Options { paths, errors_only: false, format: OutputFormat::Text };
        let mut out = Vec::new();
        let summary = run(&options, &mut out).unwrap();
        (String::from_utf8(out).unwrap(), summary)
    }

    #[test]
    fn a_file_argument_finds_the_schema_next_to_it() {
        let dir = project("neighbour");
        std::fs::copy(dir.join("schema/s.gsql"), dir.join("queries/s.gsql")).unwrap();
        let (out, summary) = check(vec![dir.join("queries/q.gsql")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.contains("no attribute `nme`"), "{out}");
        assert!(!out.contains("no-schema"), "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
        assert_eq!(summary, Summary { problems: 0, unreadable: 0 });
    }

    #[test]
    fn a_file_argument_honours_a_gsqlroot_above_it() {
        let dir = project("marked");
        let (out, _) = check(vec![dir.join("queries/q.gsql")]);
        assert!(out.contains("no-schema"), "{out}");
        std::fs::write(dir.join(".gsqlroot"), "").unwrap();
        let (out, _) = check(vec![dir.join("queries/q.gsql")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.contains("no attribute `nme`") && !out.contains("no-schema"), "{out}");
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    #[test]
    fn a_directory_argument_honours_a_gsqlroot_above_it() {
        let dir = project("marked-dir");
        let (out, _) = check(vec![dir.join("queries")]);
        assert!(out.contains("no-schema"), "{out}");
        std::fs::write(dir.join(".gsqlroot"), "").unwrap();
        std::fs::write(dir.join("schema/bad.gsql"), "CREATE QUERY b() FOR GRAPH G {\n PRINT x;\n}\n").unwrap();
        let (out, _) = check(vec![dir.join("queries")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.contains("no attribute `nme`") && !out.contains("no-schema"), "{out}");
        // Only the files under the argument are reported.
        assert_eq!(out.lines().count(), 1, "{out}");
    }

    /// Runs `check` with the current folder set to `cwd`, serialised with other
    /// tests that change it.
    fn check_in(cwd: &Path, paths: Vec<PathBuf>) -> String {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let before = std::env::current_dir().unwrap();
        std::env::set_current_dir(cwd).unwrap();
        let (out, _) = check(paths);
        std::env::set_current_dir(before).unwrap();
        out
    }

    #[test]
    fn a_project_root_is_not_a_duplicate_of_itself_under_any_spelling() {
        let dir = std::fs::canonicalize(project("spelling")).unwrap();
        std::fs::write(dir.join(".gsqlroot"), "").unwrap();
        std::fs::write(dir.join("schema/s.gsql"), "CREATE VERTEX P (PRIMARY_ID id STRING)\n").unwrap();
        std::fs::remove_file(dir.join("queries/q.gsql")).unwrap();
        let link = dir.with_file_name(format!("{}-link", dir.file_name().unwrap().to_string_lossy()));
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&dir, &link).unwrap();
        let sub = dir.join("queries");
        let cases = [
            (dir.clone(), vec![PathBuf::from(".")]),
            (dir.clone(), vec![PathBuf::from("./")]),
            (dir.clone(), vec![PathBuf::from("schema")]),
            (sub.clone(), vec![PathBuf::from("..")]),
            (sub.clone(), vec![PathBuf::from("../schema")]),
            (dir.clone(), vec![dir.clone()]),
            (dir.clone(), vec![link.clone()]),
            (dir.clone(), vec![link.join("schema/s.gsql")]),
        ];
        for (cwd, paths) in cases {
            let out = check_in(&cwd, paths.clone());
            assert!(out.is_empty(), "{paths:?} from {cwd:?}: {out}");
        }
        let _ = std::fs::remove_file(&link);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn files_that_are_not_utf8_are_still_indexed() {
        let dir = project("latin1");
        std::fs::write(dir.join(".gsqlroot"), "").unwrap();
        let mut schema = b"// Sch\xe9ma\n".to_vec();
        schema.extend_from_slice(SCHEMA.as_bytes());
        std::fs::write(dir.join("schema/s.gsql"), schema).unwrap();
        let (out, summary) = check(vec![dir.join("queries")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(out.contains("no attribute `nme`") && !out.contains("no-schema"), "{out}");
        assert_eq!(summary, Summary { problems: 0, unreadable: 0 });
    }

    #[test]
    fn a_file_named_twice_is_checked_once() {
        let dir = project("twice");
        let (out, _) = check(vec![dir.clone(), dir.join("queries/q.gsql")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(out.matches("no attribute `nme`").count(), 1, "{out}");
    }

    #[test]
    fn unreadable_paths_are_counted_and_the_rest_is_checked() {
        let dir = project("missing");
        let (out, summary) = check(vec![dir.join("nonexistent.gsql"), dir.join("queries/q.gsql")]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(summary.unreadable, 1);
        assert!(out.contains("q.gsql:1:"), "{out}");
        assert!(!out.contains("cannot read"), "errors go to stderr: {out}");
        let options =
            Options { paths: vec![dir.join("nonexistent.gsql")], errors_only: false, format: OutputFormat::Json };
        let mut json = Vec::new();
        assert_eq!(run(&options, &mut json).unwrap().unreadable, 1);
        assert_eq!(String::from_utf8(json).unwrap().trim(), "[]");
    }
}
