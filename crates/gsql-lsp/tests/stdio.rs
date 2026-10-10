//! End-to-end tests that talk to the `gsql-lsp` binary over stdio.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next_id: i64,
    /// The diagnostics last published for each document.
    published: HashMap<String, Vec<Value>>,
}

fn read_message(reader: &mut BufReader<ChildStdout>) -> Option<Value> {
    let mut length = 0;
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).ok()? == 0 {
            return None;
        }
        let line = line.trim_end();
        if line.is_empty() {
            break;
        }
        if let Some(value) = line.strip_prefix("Content-Length: ") {
            length = value.parse().ok()?;
        }
    }
    let mut body = vec![0; length];
    reader.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}

/// The document and diagnostics of a `publishDiagnostics` notification.
fn published_diagnostics(message: &Value) -> Option<(String, Vec<Value>)> {
    if message["method"] != "textDocument/publishDiagnostics" {
        return None;
    }
    let uri = message["params"]["uri"]
        .as_str()?
        .to_string();
    let diagnostics = message["params"]["diagnostics"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    Some((uri, diagnostics))
}

impl Client {
    fn start(root: &Path) -> Client {
        Client::start_with(
            root,
            json!({
                "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } },
                "general": { "positionEncodings": ["utf-16"] },
            }),
        )
    }

    fn start_with(root: &Path, capabilities: Value) -> Client {
        let mut child = Command::new(env!("CARGO_BIN_EXE_gsql-lsp"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("start gsql-lsp");
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let (sender, messages) = mpsc::channel();
        std::thread::spawn(move || {
            while let Some(message) = read_message(&mut stdout) {
                if sender.send(message).is_err() {
                    break;
                }
            }
        });
        let mut client = Client {
            child,
            stdin,
            messages,
            next_id: 1,
            published: HashMap::new(),
        };
        let root_uri = gsql_lsp::uri::from_path(root);
        let result = client.request(
            "initialize",
            json!({
                "processId": null,
                "rootUri": root_uri,
                "capabilities": capabilities,
            }),
        );
        assert!(result["capabilities"]["positionEncoding"].is_string());
        client.notify("initialized", json!({}));
        client
    }

    fn send(&mut self, message: Value) {
        let body = serde_json::to_string(&message).unwrap();
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body)
            .unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(
            json!({ "jsonrpc": "2.0", "method": method, "params": params }),
        );
    }

    fn receive(&mut self) -> Value {
        let message = self
            .messages
            .recv_timeout(Duration::from_secs(10))
            .expect("message from the server");
        if let Some((uri, diagnostics)) = published_diagnostics(&message) {
            self.published.insert(uri, diagnostics);
        }
        message
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(
            json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }),
        );
        loop {
            let message = self.receive();
            if message["id"] == json!(id) && message.get("method").is_none() {
                assert!(
                    message.get("error").is_none(),
                    "{method} failed: {message}"
                );
                return message["result"].clone();
            }
        }
    }

    /// Waits for diagnostics for `uri` that satisfy `accept`.
    fn diagnostics(
        &mut self,
        uri: &str,
        accept: impl Fn(&[Value]) -> bool,
    ) -> Vec<Value> {
        loop {
            let message = self.receive();
            if let Some((from, diagnostics)) = published_diagnostics(&message)
                && from == uri
                && accept(&diagnostics)
            {
                return diagnostics;
            }
        }
    }

    /// The diagnostic codes of `uri` once the server has handled everything sent.
    fn codes(&mut self, uri: &str) -> Vec<String> {
        self.request("workspace/symbol", json!({ "query": "" }));
        let diagnostics = &self.published[uri];
        diagnostics
            .iter()
            .filter_map(|d| d["code"].as_str().map(String::from))
            .collect()
    }

    /// Waits until the server has indexed the workspace folders it was last given.
    fn indexed(&mut self) {
        loop {
            let message = self.receive();
            if message["method"] == "window/logMessage"
                && message["params"]["message"]
                    .as_str()
                    .is_some_and(|m| m.starts_with("gsql-lsp: indexed "))
            {
                return;
            }
        }
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "gsql", "version": 1, "text": text } }),
        );
    }

    fn close(&mut self, uri: &str) {
        self.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": uri } }),
        );
    }

    fn hover(&mut self, uri: &str, line: u32, character: u32) -> Value {
        let position = json!({ "line": line, "character": character });
        self.request(
            "textDocument/hover",
            json!({ "textDocument": { "uri": uri }, "position": position }),
        )
    }

    fn change_folders(&mut self, added: &[&Path], removed: &[&Path]) {
        let folders = |paths: &[&Path]| -> Vec<Value> {
            paths
                .iter()
                .map(|p| json!({ "uri": gsql_lsp::uri::from_path(p) }))
                .collect()
        };
        self.notify(
            "workspace/didChangeWorkspaceFolders",
            json!({ "event": { "added": folders(added), "removed": folders(removed) } }),
        );
    }

    fn shutdown(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
    }
}

fn temp_workspace(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir()
        .join(format!("gsql-lsp-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (file, text) in files {
        std::fs::write(dir.join(file), text).unwrap();
    }
    dir
}

const SCHEMA: &str = concat!(
    "CREATE VERTEX Person (PRIMARY_ID id STRING, name STRING, age INT)\n",
    "CREATE UNDIRECTED EDGE Friendship (FROM Person, TO Person, since DATETIME)\n",
    "CREATE GRAPH Social (Person, Friendship)\n",
);

/// A query that needs `SCHEMA`: without it, `Person` gets a `no-schema` warning.
const QUERY: &str =
    "CREATE QUERY q() FOR GRAPH Social {\n  S = {Person.*};\n  PRINT S;\n}\n";

#[test]
fn serves_a_session_end_to_end() {
    let root = temp_workspace("session", &[("schema.gsql", SCHEMA)]);
    let mut client = Client::start(&root);
    let uri = gsql_lsp::uri::from_path(&root.join("query.gsql"));
    let text = [
        "CREATE QUERY friends(VERTEX<Person> p) FOR GRAPH Social {",
        "  SumAccum<INT> @@count;",
        "  Start = {p};",
        "  Result = SELECT t FROM Start:s -(Friendship:e)- Person:t",
        "           WHERE t.agee > 18",
        "           ACCUM @@count += 1;",
        "  PRINT Result, @@count;",
        "}",
        "",
    ]
    .join("\n");
    let text = text.as_str();
    client.open(&uri, text);
    // Diagnostics arrive once the workspace (with the schema file) is indexed.
    let diagnostics = client.diagnostics(&uri, |d| !d.is_empty());
    let messages: Vec<&str> = diagnostics
        .iter()
        .filter_map(|d| d["message"].as_str())
        .collect();
    assert_eq!(
        messages,
        vec!["`Person` has no attribute `agee` (did you mean `age`?)"]
    );

    let hover = client.request(
        "textDocument/hover",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 5, "character": 18 } }),
    );
    let markdown = hover["contents"]["value"].as_str().unwrap();
    assert!(markdown.contains("SumAccum<INT> @@count"), "{markdown}");

    let completion = client.request(
        "textDocument/completion",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 4, "character": 19 } }),
    );
    let labels: Vec<&str> = completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["label"].as_str())
        .collect();
    assert!(
        labels.contains(&"age") && labels.contains(&"name"),
        "{labels:?}"
    );

    let definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 3, "character": 52 } }),
    );
    assert_eq!(
        definition[0]["uri"],
        gsql_lsp::uri::from_path(&root.join("schema.gsql"))
    );

    // Fix the typo with an incremental change and expect clean diagnostics.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "range": { "start": { "line": 4, "character": 19 }, "end": { "line": 4, "character": 23 } },
                "text": "age",
            }],
        }),
    );
    let diagnostics = client.diagnostics(&uri, |_| true);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");

    let tokens = client.request(
        "textDocument/semanticTokens/full",
        json!({ "textDocument": { "uri": uri } }),
    );
    assert!(tokens["data"].as_array().unwrap().len() > 20);

    let symbols =
        client.request("workspace/symbol", json!({ "query": "Friend" }));
    assert!(
        symbols
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["name"] == "Friendship")
    );

    client.shutdown();
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn rejects_requests_before_initialize_and_survives_bad_input() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_gsql-lsp"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut send = |text: &str| {
        write!(stdin, "Content-Length: {}\r\n\r\n{}", text.len(), text)
            .unwrap();
        stdin.flush().unwrap();
    };
    send("{not json}");
    send(
        r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":{}}"#,
    );
    // Broken JSON gets a parse error (with a null id), then the session goes on.
    let response = read_message(&mut stdout).unwrap();
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["id"].is_null());
    let response = read_message(&mut stdout).unwrap();
    assert_eq!(response["error"]["code"], -32002);
    send(r#"{"jsonrpc":"2.0","id":2,"method":5}"#);
    let response = read_message(&mut stdout).unwrap();
    assert_eq!(
        (response["id"].clone(), response["error"]["code"].clone()),
        (json!(2), json!(-32600))
    );
    send(r#"{"jsonrpc":"2.0","method":"exit"}"#);
    let status = child.wait().unwrap();
    // Exiting without a shutdown request reports failure, per the protocol.
    assert_eq!(status.code(), Some(1));
}

/// The type of a pattern alias follows the ends of the edge in the schema: editing
/// only an edge's ends must refresh the diagnostics of the other open files.
#[test]
fn editing_an_edge_end_retypes_aliases_in_other_files() {
    const SCHEMA: &str = concat!(
        "CREATE VERTEX Person (PRIMARY_ID id STRING)\n",
        "CREATE VERTEX Company (PRIMARY_ID id STRING, title STRING)\n",
        "CREATE VERTEX City (PRIMARY_ID id STRING, pop INT)\n",
        "CREATE DIRECTED EDGE works_at (FROM Person, TO Company)\n",
    );
    let query = "CREATE QUERY q() {\n  S = {Person.*};\n  R = SELECT m FROM S:s -(works_at>)- :m WHERE m.title == \"x\";\n  PRINT R;\n}\n";
    let dir = temp_workspace(
        "edge-ends",
        &[("schema.gsql", SCHEMA), ("q.gsql", query)],
    );
    let mut client = Client::start(&dir);
    let schema_uri = gsql_lsp::uri::from_path(&dir.join("schema.gsql"));
    let query_uri = gsql_lsp::uri::from_path(&dir.join("q.gsql"));
    client.open(&schema_uri, SCHEMA);
    client.open(&query_uri, query);
    let found = client.diagnostics(&query_uri, |d| {
        d.iter().all(|x| {
            !x["message"]
                .as_str()
                .unwrap_or("")
                .contains("no attribute")
        })
    });
    assert!(
        found
            .iter()
            .all(|d| d["code"] != "unknown-attribute"),
        "{found:?}"
    );
    // The edge now ends at City, which has no `title`.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": schema_uri, "version": 2 },
            "contentChanges": [{ "text": SCHEMA.replace("TO Company", "TO City") }],
        }),
    );
    let found = client.diagnostics(&query_uri, |d| {
        d.iter()
            .any(|x| x["code"] == "unknown-attribute")
    });
    assert!(
        found.iter().any(|d| d["message"]
            .as_str()
            .unwrap_or("")
            .contains("title")),
        "{found:?}"
    );
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn removing_a_folder_keeps_its_open_documents_indexed() {
    let root = temp_workspace(
        "removed",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    client.open(&gsql_lsp::uri::from_path(&root.join("schema.gsql")), SCHEMA);
    let query = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&query, QUERY);
    assert!(!client.hover(&query, 1, 8).is_null());
    client.change_folders(&[], &[&root]);
    still_finds_the_schema(client, &query, &[&root]);
}

#[test]
fn removing_a_folder_keeps_the_closed_neighbours_of_its_open_documents() {
    let root = temp_workspace(
        "left-out",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&query, QUERY);
    assert_eq!(client.codes(&query), Vec::<String>::new());
    client.change_folders(&[], &[&root]);
    still_finds_the_schema(client, &query, &[&root]);
}

#[test]
fn closing_a_document_after_its_folder_is_removed_keeps_it_indexed() {
    let root = temp_workspace(
        "closed-out",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    let schema = gsql_lsp::uri::from_path(&root.join("schema.gsql"));
    client.open(&schema, SCHEMA);
    let query = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&query, QUERY);
    client.change_folders(&[], &[&root]);
    client.close(&schema);
    still_finds_the_schema(client, &query, &[&root]);
}

#[test]
fn removing_a_nested_folder_keeps_the_files_of_the_outer_one() {
    let root = temp_workspace("nested", &[("q.gsql", QUERY)]);
    let sub = root.join("sub");
    std::fs::create_dir_all(&sub).unwrap();
    std::fs::write(sub.join("schema.gsql"), SCHEMA).unwrap();
    let mut client = Client::start(&root);
    client.indexed();
    client.change_folders(&[&sub], &[]);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&query, QUERY);
    assert_eq!(client.codes(&query), Vec::<String>::new());
    client.change_folders(&[], &[&sub]);
    still_finds_the_schema(client, &query, &[&root]);
}

#[test]
fn closing_a_file_of_a_loose_project_keeps_it_indexed() {
    let root = temp_workspace("loose-root", &[]);
    let loose = temp_workspace(
        "loose",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    let schema = gsql_lsp::uri::from_path(&loose.join("schema.gsql"));
    client.open(&schema, SCHEMA);
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    assert!(!client.hover(&query, 1, 8).is_null());
    client.close(&schema);
    still_finds_the_schema(client, &query, &[&root, &loose]);
}

#[test]
fn removing_the_added_folder_of_a_loose_file_keeps_its_neighbours() {
    let root = temp_workspace("readded-root", &[]);
    let loose = temp_workspace(
        "readded",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    assert_eq!(client.codes(&query), Vec::<String>::new());
    client.change_folders(&[&loose], &[]);
    client.indexed();
    client.change_folders(&[], &[&loose]);
    still_finds_the_schema(client, &query, &[&root, &loose]);
}

/// Asserts that the open `query` still finds `Person` once `dirs` are deleted, then
/// shuts the server down.
fn still_finds_the_schema(
    mut client: Client,
    query: &str,
    dirs: &[&PathBuf],
) {
    let codes = client.codes(query);
    for dir in dirs {
        std::fs::remove_dir_all(dir).unwrap();
    }
    assert_eq!(codes, Vec::<String>::new());
    assert!(
        !client.hover(query, 1, 8).is_null(),
        "Person is still known"
    );
    client.shutdown();
}

/// The URIs of the workspace symbols named like `query`.
fn symbol_uris(client: &mut Client, query: &str) -> Vec<String> {
    let symbols =
        client.request("workspace/symbol", json!({ "query": query }));
    symbols
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|s| {
            s["location"]["uri"]
                .as_str()
                .map(String::from)
        })
        .collect()
}

#[test]
fn closing_the_last_document_of_a_removed_folder_drops_the_folder() {
    let root =
        temp_workspace("kept", &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)]);
    // (Its URIs sort first, so its query would be the first definition of `q`.)
    let old = temp_workspace(
        "gone",
        &[("old_schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    client.change_folders(&[&old], &[]);
    client.indexed();
    let old_query = gsql_lsp::uri::from_path(&old.join("q.gsql"));
    client.open(&old_query, QUERY);
    client.change_folders(&[], &[&old]);
    client.close(&old_query);
    let uris = symbol_uris(&mut client, "Person");
    let query = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&query, QUERY);
    let codes = client.codes(&query);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&old).unwrap();
    let schema = gsql_lsp::uri::from_path(&root.join("schema.gsql"));
    assert_eq!(uris, vec![schema]);
    // No duplicate-definition hint pointing at the removed folder's query.
    assert_eq!(codes, Vec::<String>::new());
    client.shutdown();
}

#[test]
fn closing_a_document_of_a_deleted_marked_project_drops_the_project() {
    let root = temp_workspace(
        "kept-marked",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let old = temp_workspace(
        "gone-marked",
        &[(".gsqlroot", ""), ("old_schema.gsql", SCHEMA)],
    );
    std::fs::create_dir_all(old.join("sub")).unwrap();
    std::fs::write(old.join("sub").join("q.gsql"), QUERY).unwrap();
    let mut client = Client::start(&root);
    client.indexed();
    client.change_folders(&[&old], &[]);
    client.indexed();
    let old_query = gsql_lsp::uri::from_path(&old.join("sub").join("q.gsql"));
    client.open(&old_query, QUERY);
    client.change_folders(&[], &[&old]);
    let held = symbol_uris(&mut client, "Person");
    std::fs::remove_dir_all(&old).unwrap();
    client.close(&old_query);
    let uris = symbol_uris(&mut client, "Person");
    let schema = gsql_lsp::uri::from_path(&root.join("schema.gsql"));
    client.open(&schema, SCHEMA);
    let codes = client.codes(&schema);
    std::fs::remove_dir_all(&root).unwrap();
    assert_eq!(held.len(), 2, "the project is held while open: {held:?}");
    assert_eq!(uris, vec![schema]);
    // No duplicate-definition hint pointing at the deleted schema.
    assert_eq!(codes, Vec::<String>::new());
    client.shutdown();
}

/// An empty workspace folder, a folder `outer`, and the loose project `outer/loose`.
fn nested_loose_project(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = temp_workspace(&format!("{name}-root"), &[]);
    let outer = temp_workspace(name, &[]);
    let loose = outer.join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    std::fs::write(loose.join("schema.gsql"), SCHEMA).unwrap();
    std::fs::write(loose.join("q.gsql"), QUERY).unwrap();
    (root, outer, loose)
}

#[test]
fn closing_a_loose_document_after_a_marker_appears_drops_its_project() {
    let (root, outer, loose) = nested_loose_project("marked-later");
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    let held = symbol_uris(&mut client, "Person");
    std::fs::write(outer.join(".gsqlroot"), "").unwrap();
    client.close(&query);
    let uris = symbol_uris(&mut client, "Person");
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&outer).unwrap();
    let schema = gsql_lsp::uri::from_path(&loose.join("schema.gsql"));
    assert_eq!(held, vec![schema], "the project is held while open");
    assert_eq!(uris, Vec::<String>::new());
    client.shutdown();
}

#[test]
fn a_loose_document_moved_to_another_project_releases_both() {
    let (root, outer, loose) = nested_loose_project("moved");
    let other = temp_workspace("moved-other", &[]);
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    symbol_uris(&mut client, "Person");
    let marker = outer.join(".gsqlroot");
    std::fs::write(&marker, "").unwrap();
    // The project of each loose document is looked up again: now `outer`.
    client.change_folders(&[&other], &[]);
    client.indexed();
    let held = symbol_uris(&mut client, "Person");
    // With the marker gone, a stale `outer/loose` would still hold the schema.
    std::fs::remove_file(&marker).unwrap();
    client.close(&query);
    let uris = symbol_uris(&mut client, "Person");
    for dir in [&root, &outer, &other] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    let schema = gsql_lsp::uri::from_path(&loose.join("schema.gsql"));
    assert_eq!(held, vec![schema], "the new project is held while open");
    assert_eq!(uris, Vec::<String>::new());
    client.shutdown();
}

#[test]
fn loose_documents_opened_across_a_marker_change_share_their_project() {
    for (name, marker_first) in
        [("marker-comes", false), ("marker-goes", true)]
    {
        let (root, outer, loose) = nested_loose_project(name);
        let marker = outer.join(".gsqlroot");
        if marker_first {
            std::fs::write(&marker, "").unwrap();
        }
        let mut client = Client::start(&root);
        client.indexed();
        let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
        client.open(&query, QUERY);
        symbol_uris(&mut client, "Person");
        if marker_first {
            std::fs::remove_file(&marker).unwrap();
        } else {
            std::fs::write(&marker, "").unwrap();
        }
        let other = gsql_lsp::uri::from_path(&loose.join("b.gsql"));
        client.open(&other, "");
        client.close(&other);
        let held = symbol_uris(&mut client, "Person");
        let codes = client.codes(&query);
        client.close(&query);
        let left = symbol_uris(&mut client, "Person");
        for dir in [&root, &outer] {
            std::fs::remove_dir_all(dir).unwrap();
        }
        let schema = gsql_lsp::uri::from_path(&loose.join("schema.gsql"));
        assert_eq!(held, vec![schema], "{name}: held while one is open");
        assert_eq!(codes, Vec::<String>::new(), "{name}");
        assert_eq!(
            left,
            Vec::<String>::new(),
            "{name}: dropped when all close"
        );
        client.shutdown();
    }
}

#[test]
fn a_loose_project_closed_inside_a_folder_leaves_with_the_folder() {
    let root = temp_workspace("closed-in-root", &[]);
    let loose = temp_workspace(
        "closed-in",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    client.change_folders(&[&loose], &[]);
    client.indexed();
    client.close(&query);
    client.change_folders(&[], &[&loose]);
    let uris = symbol_uris(&mut client, "Person");
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&loose).unwrap();
    assert_eq!(uris, Vec::<String>::new());
    client.shutdown();
}

#[test]
fn a_loose_project_file_changed_on_disk_is_indexed_again() {
    let root = temp_workspace("watched-root", &[]);
    let loose = temp_workspace(
        "watched",
        &[("schema.gsql", SCHEMA), ("q.gsql", QUERY)],
    );
    let mut client = Client::start(&root);
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    client.open(&query, QUERY);
    assert!(!client.hover(&query, 1, 8).is_null());
    let schema = loose.join("schema.gsql");
    std::fs::write(&schema, SCHEMA.replace("Person", "Human")).unwrap();
    client.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": gsql_lsp::uri::from_path(&schema), "type": 2 }] }),
    );
    let hover = client.hover(&query, 1, 8);
    std::fs::remove_dir_all(&root).unwrap();
    std::fs::remove_dir_all(&loose).unwrap();
    assert!(hover.is_null(), "Person is gone: {hover}");
    client.shutdown();
}

const FORMATS_QUERY: &str = concat!(
    "CREATE QUERY q() FOR GRAPH Social {\n",
    "  SumAccum<INT> @@count;\n",
    "  PRINT @@count, abs(1);\n",
    "}\n",
);

struct Answers {
    hover: Value,
    completion: Value,
    signature: Value,
    symbols: Value,
    workspace_symbols: Value,
}

fn ask(name: &str, capabilities: Value) -> Answers {
    let root = temp_workspace(name, &[("schema.gsql", SCHEMA)]);
    let mut client = Client::start_with(&root, capabilities);
    let uri = gsql_lsp::uri::from_path(&root.join("q.gsql"));
    client.open(&uri, FORMATS_QUERY);
    let at = |line: u32, character: u32| json!({ "textDocument": { "uri": uri }, "position": { "line": line, "character": character } });
    let answers = Answers {
        hover: client.request("textDocument/hover", at(2, 10)),
        completion: client.request("textDocument/completion", at(2, 2)),
        signature: client.request("textDocument/signatureHelp", at(2, 22)),
        symbols: client.request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": uri } }),
        ),
        workspace_symbols: client
            .request("workspace/symbol", json!({ "query": "Person" })),
    };
    client.shutdown();
    let _ = std::fs::remove_dir_all(root);
    answers
}

#[test]
fn a_full_client_gets_markdown_snippets_and_hierarchical_symbols() {
    let a = ask(
        "formats-full",
        json!({ "textDocument": {
            "hover": { "contentFormat": ["markdown", "plaintext"] },
            "completion": { "completionItem": { "snippetSupport": true, "documentationFormat": ["markdown", "plaintext"] } },
            "signatureHelp": { "signatureInformation": { "documentationFormat": ["markdown", "plaintext"] } },
            "documentSymbol": { "hierarchicalDocumentSymbolSupport": true },
        }}),
    );
    assert_eq!(a.hover["contents"]["kind"], "markdown");
    assert!(
        a.hover["contents"]["value"]
            .as_str()
            .unwrap()
            .contains("```")
    );
    let items = a.completion["items"].as_array().unwrap();
    assert!(
        items
            .iter()
            .any(|i| i["insertTextFormat"] == 2 || i["textEdit"].is_object())
    );
    assert!(
        items
            .iter()
            .any(|i| i["insertTextFormat"] == 2),
        "snippets expected"
    );
    assert!(
        items
            .iter()
            .filter(|i| i["documentation"].is_object())
            .all(|i| i["documentation"]["kind"] == "markdown")
    );
    let signature = &a.signature["signatures"][0];
    assert_eq!(signature["documentation"]["kind"], "markdown");
    // Hierarchical: DocumentSymbol with a range, the query nested in nothing, accumulator as a child.
    let query = &a.symbols.as_array().unwrap()[0];
    assert!(
        query["selectionRange"].is_object() && query["location"].is_null()
    );
    assert!(
        query["children"]
            .as_array()
            .is_some_and(|c| !c.is_empty()),
        "{query}"
    );
    assert!(
        a.workspace_symbols
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["location"].is_object())
    );
}

#[test]
fn a_plaintext_only_client_gets_plain_text() {
    let a = ask(
        "formats-plain",
        json!({ "textDocument": {
            "hover": { "contentFormat": ["plaintext"] },
            "completion": { "completionItem": { "documentationFormat": ["plaintext"] } },
            "signatureHelp": { "signatureInformation": { "documentationFormat": ["plaintext"] } },
        }}),
    );
    assert_eq!(a.hover["contents"]["kind"], "plaintext");
    let text = a.hover["contents"]["value"]
        .as_str()
        .unwrap();
    assert!(
        text.contains("SumAccum<INT> @@count")
            && !text.contains("```")
            && !text.contains("**"),
        "{text}"
    );
    let documented: Vec<&Value> = a.completion["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["documentation"].is_object())
        .collect();
    assert!(!documented.is_empty());
    for item in documented {
        let doc = &item["documentation"];
        assert_eq!(doc["kind"], "plaintext");
        assert!(
            !doc["value"]
                .as_str()
                .unwrap()
                .contains("```"),
            "{item}"
        );
    }
    let signature = &a.signature["signatures"][0];
    assert_eq!(signature["documentation"]["kind"], "plaintext");
    assert!(
        !signature["documentation"]["value"]
            .as_str()
            .unwrap()
            .contains("```")
    );
}

#[test]
fn a_client_without_hierarchical_symbols_gets_the_flat_form() {
    let a = ask(
        "formats-flat",
        json!({ "textDocument": { "documentSymbol": { "dynamicRegistration": false } } }),
    );
    let symbols = a.symbols.as_array().unwrap();
    assert!(
        symbols
            .iter()
            .all(|s| s["location"]["uri"].is_string()
                && s.get("children").is_none()),
        "{symbols:?}"
    );
    let count = symbols
        .iter()
        .find(|s| s["name"] == "@@count")
        .expect("accumulator listed");
    assert_eq!(count["containerName"], "q");
    // A client that declares nothing at all also gets the flat form, and Markdown.
    let b = ask("formats-none", json!({}));
    assert!(
        b.symbols
            .as_array()
            .unwrap()
            .iter()
            .all(|s| s["location"].is_object())
    );
    assert_eq!(b.hover["contents"]["kind"], "markdown");
}

#[test]
fn a_client_without_snippets_gets_no_snippets() {
    let a = ask(
        "formats-nosnippets",
        json!({ "textDocument": { "completion": { "completionItem": { "snippetSupport": false } } } }),
    );
    let items = a.completion["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for item in items {
        assert_ne!(item["insertTextFormat"], 2, "{item}");
        assert_ne!(item["kind"], 15, "{item}");
        for text in [
            item["insertText"].as_str(),
            item["textEdit"]["newText"].as_str(),
        ]
        .into_iter()
        .flatten()
        {
            assert!(!text.contains("${") && !text.contains("$1"), "{item}");
        }
    }
}

/// A loose file reopened after a `.gsqlroot` came and went finds its schema.
#[test]
fn a_loose_file_reopened_after_a_marker_came_and_went_finds_its_schema() {
    let (root, outer, loose) = nested_loose_project("marker-flips");
    std::fs::remove_file(loose.join("q.gsql")).unwrap();
    let marker = outer.join(".gsqlroot");
    std::fs::write(&marker, "").unwrap();
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    let other = gsql_lsp::uri::from_path(&loose.join("b.gsql"));
    client.open(&query, QUERY);
    symbol_uris(&mut client, "Person");
    std::fs::remove_file(&marker).unwrap();
    client.open(&other, "");
    client.close(&other);
    symbol_uris(&mut client, "Person");
    std::fs::write(&marker, "").unwrap();
    client.close(&query);
    symbol_uris(&mut client, "Person");
    std::fs::remove_file(&marker).unwrap();
    client.open(&query, QUERY);
    let uris = symbol_uris(&mut client, "Person");
    let codes = client.codes(&query);
    for dir in [&root, &outer] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    assert_eq!(
        uris,
        vec![gsql_lsp::uri::from_path(&loose.join("schema.gsql"))]
    );
    assert_eq!(codes, Vec::<String>::new());
    client.shutdown();
}

/// Closing every loose file drops a project a `.gsqlroot` change left behind.
#[test]
fn closing_every_loose_file_drops_a_project_a_marker_change_left_behind() {
    let root = temp_workspace("marker-leaks-root", &[]);
    let outer =
        temp_workspace("marker-leaks", &[("old_schema.gsql", SCHEMA)]);
    let loose = outer.join("loose");
    std::fs::create_dir_all(&loose).unwrap();
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&loose.join("q.gsql"));
    let other = gsql_lsp::uri::from_path(&loose.join("b.gsql"));
    client.open(&query, QUERY);
    symbol_uris(&mut client, "Person");
    let marker = outer.join(".gsqlroot");
    std::fs::write(&marker, "").unwrap();
    client.open(&other, "");
    client.close(&other);
    let held = symbol_uris(&mut client, "Person");
    std::fs::remove_file(&marker).unwrap();
    client.close(&query);
    let uris = symbol_uris(&mut client, "Person");
    for dir in [&root, &outer] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    let schema = gsql_lsp::uri::from_path(&outer.join("old_schema.gsql"));
    assert_eq!(held, vec![schema], "held while one is open");
    assert_eq!(uris, Vec::<String>::new());
    client.shutdown();
}

/// Closing a file of a marked project nested in another keeps the outer one whole.
#[test]
fn closing_a_file_of_a_nested_marked_project_keeps_the_outer_one_whole() {
    let root = temp_workspace("nested-marked-root", &[]);
    let outer = temp_workspace("nested-marked", &[(".gsqlroot", "")]);
    let inner = outer.join("inner");
    std::fs::create_dir_all(&inner).unwrap();
    std::fs::write(inner.join(".gsqlroot"), "").unwrap();
    std::fs::write(inner.join("schema.gsql"), SCHEMA).unwrap();
    let mut client = Client::start(&root);
    client.indexed();
    let query = gsql_lsp::uri::from_path(&outer.join("q.gsql"));
    let other = gsql_lsp::uri::from_path(&inner.join("b.gsql"));
    client.open(&query, QUERY);
    client.open(&other, "");
    client.close(&other);
    let uris = symbol_uris(&mut client, "Person");
    let codes = client.codes(&query);
    for dir in [&root, &outer] {
        std::fs::remove_dir_all(dir).unwrap();
    }
    // The outer project indexed the inner one's files too: they stay.
    assert_eq!(
        uris,
        vec![gsql_lsp::uri::from_path(&inner.join("schema.gsql"))]
    );
    assert_eq!(codes, Vec::<String>::new());
    client.shutdown();
}
