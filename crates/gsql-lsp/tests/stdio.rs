//! End-to-end tests that talk to the `gsql-lsp` binary over stdio.

use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use serde_json::{Value, json};

struct Client {
    child: Child,
    stdin: ChildStdin,
    messages: Receiver<Value>,
    next_id: i64,
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

impl Client {
    fn start(root: &std::path::Path) -> Client {
        Client::start_with(
            root,
            json!({
                "textDocument": { "completion": { "completionItem": { "snippetSupport": true } } },
                "general": { "positionEncodings": ["utf-16"] },
            }),
        )
    }

    fn start_with(root: &std::path::Path, capabilities: Value) -> Client {
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
        let mut client = Client { child, stdin, messages, next_id: 1 };
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
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", body.len(), body).unwrap();
        self.stdin.flush().unwrap();
    }

    fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({ "jsonrpc": "2.0", "method": method, "params": params }));
    }

    fn receive(&mut self) -> Value {
        self.messages.recv_timeout(Duration::from_secs(10)).expect("message from the server")
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params }));
        loop {
            let message = self.receive();
            if message["id"] == json!(id) && message.get("method").is_none() {
                assert!(message.get("error").is_none(), "{method} failed: {message}");
                return message["result"].clone();
            }
        }
    }

    /// Waits for diagnostics for `uri` that satisfy `accept`.
    fn diagnostics(&mut self, uri: &str, accept: impl Fn(&[Value]) -> bool) -> Vec<Value> {
        loop {
            let message = self.receive();
            if message["method"] == "textDocument/publishDiagnostics" && message["params"]["uri"] == uri {
                let diagnostics = message["params"]["diagnostics"].as_array().cloned().unwrap_or_default();
                if accept(&diagnostics) {
                    return diagnostics;
                }
            }
        }
    }

    fn open(&mut self, uri: &str, text: &str) {
        self.notify(
            "textDocument/didOpen",
            json!({ "textDocument": { "uri": uri, "languageId": "gsql", "version": 1, "text": text } }),
        );
    }

    fn shutdown(mut self) {
        self.request("shutdown", Value::Null);
        self.notify("exit", Value::Null);
        let status = self.child.wait().unwrap();
        assert!(status.success(), "server exited with {status}");
    }
}

fn temp_workspace(name: &str, files: &[(&str, &str)]) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("gsql-lsp-test-{name}-{}", std::process::id()));
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
    let messages: Vec<&str> = diagnostics.iter().filter_map(|d| d["message"].as_str()).collect();
    assert_eq!(messages, vec!["`Person` has no attribute `agee` (did you mean `age`?)"]);

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
    let labels: Vec<&str> =
        completion["items"].as_array().unwrap().iter().filter_map(|i| i["label"].as_str()).collect();
    assert!(labels.contains(&"age") && labels.contains(&"name"), "{labels:?}");

    let definition = client.request(
        "textDocument/definition",
        json!({ "textDocument": { "uri": uri }, "position": { "line": 3, "character": 52 } }),
    );
    assert_eq!(definition[0]["uri"], gsql_lsp::uri::from_path(&root.join("schema.gsql")));

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

    let tokens = client.request("textDocument/semanticTokens/full", json!({ "textDocument": { "uri": uri } }));
    assert!(tokens["data"].as_array().unwrap().len() > 20);

    let symbols = client.request("workspace/symbol", json!({ "query": "Friend" }));
    assert!(symbols.as_array().unwrap().iter().any(|s| s["name"] == "Friendship"));

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
        write!(stdin, "Content-Length: {}\r\n\r\n{}", text.len(), text).unwrap();
        stdin.flush().unwrap();
    };
    send("{not json}");
    send(r#"{"jsonrpc":"2.0","id":1,"method":"textDocument/hover","params":{}}"#);
    // Broken JSON gets a parse error (with a null id), then the session goes on.
    let response = read_message(&mut stdout).unwrap();
    assert_eq!(response["error"]["code"], -32700);
    assert!(response["id"].is_null());
    let response = read_message(&mut stdout).unwrap();
    assert_eq!(response["error"]["code"], -32002);
    send(r#"{"jsonrpc":"2.0","id":2,"method":5}"#);
    let response = read_message(&mut stdout).unwrap();
    assert_eq!((response["id"].clone(), response["error"]["code"].clone()), (json!(2), json!(-32600)));
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
    let dir = temp_workspace("edge-ends", &[("schema.gsql", SCHEMA), ("q.gsql", query)]);
    let mut client = Client::start(&dir);
    let schema_uri = gsql_lsp::uri::from_path(&dir.join("schema.gsql"));
    let query_uri = gsql_lsp::uri::from_path(&dir.join("q.gsql"));
    client.open(&schema_uri, SCHEMA);
    client.open(&query_uri, query);
    let found = client
        .diagnostics(&query_uri, |d| d.iter().all(|x| !x["message"].as_str().unwrap_or("").contains("no attribute")));
    assert!(found.iter().all(|d| d["code"] != "unknown-attribute"), "{found:?}");
    // The edge now ends at City, which has no `title`.
    client.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": schema_uri, "version": 2 },
            "contentChanges": [{ "text": SCHEMA.replace("TO Company", "TO City") }],
        }),
    );
    let found = client.diagnostics(&query_uri, |d| d.iter().any(|x| x["code"] == "unknown-attribute"));
    assert!(found.iter().any(|d| d["message"].as_str().unwrap_or("").contains("title")), "{found:?}");
    client.shutdown();
    let _ = std::fs::remove_dir_all(&dir);
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
        symbols: client.request("textDocument/documentSymbol", json!({ "textDocument": { "uri": uri } })),
        workspace_symbols: client.request("workspace/symbol", json!({ "query": "Person" })),
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
    assert!(a.hover["contents"]["value"].as_str().unwrap().contains("```"));
    let items = a.completion["items"].as_array().unwrap();
    assert!(items.iter().any(|i| i["insertTextFormat"] == 2 || i["textEdit"].is_object()));
    assert!(items.iter().any(|i| i["insertTextFormat"] == 2), "snippets expected");
    assert!(items.iter().filter(|i| i["documentation"].is_object()).all(|i| i["documentation"]["kind"] == "markdown"));
    let signature = &a.signature["signatures"][0];
    assert_eq!(signature["documentation"]["kind"], "markdown");
    // Hierarchical: DocumentSymbol with a range, the query nested in nothing, accumulator as a child.
    let query = &a.symbols.as_array().unwrap()[0];
    assert!(query["selectionRange"].is_object() && query["location"].is_null());
    assert!(query["children"].as_array().is_some_and(|c| !c.is_empty()), "{query}");
    assert!(a.workspace_symbols.as_array().unwrap().iter().all(|s| s["location"].is_object()));
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
    let text = a.hover["contents"]["value"].as_str().unwrap();
    assert!(text.contains("SumAccum<INT> @@count") && !text.contains("```") && !text.contains("**"), "{text}");
    let documented: Vec<&Value> =
        a.completion["items"].as_array().unwrap().iter().filter(|i| i["documentation"].is_object()).collect();
    assert!(!documented.is_empty());
    for item in documented {
        let doc = &item["documentation"];
        assert_eq!(doc["kind"], "plaintext");
        assert!(!doc["value"].as_str().unwrap().contains("```"), "{item}");
    }
    let signature = &a.signature["signatures"][0];
    assert_eq!(signature["documentation"]["kind"], "plaintext");
    assert!(!signature["documentation"]["value"].as_str().unwrap().contains("```"));
}

#[test]
fn a_client_without_hierarchical_symbols_gets_the_flat_form() {
    let a = ask("formats-flat", json!({ "textDocument": { "documentSymbol": { "dynamicRegistration": false } } }));
    let symbols = a.symbols.as_array().unwrap();
    assert!(symbols.iter().all(|s| s["location"]["uri"].is_string() && s.get("children").is_none()), "{symbols:?}");
    let count = symbols.iter().find(|s| s["name"] == "@@count").expect("accumulator listed");
    assert_eq!(count["containerName"], "q");
    // A client that declares nothing at all also gets the flat form, and Markdown.
    let b = ask("formats-none", json!({}));
    assert!(b.symbols.as_array().unwrap().iter().all(|s| s["location"].is_object()));
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
        for text in [item["insertText"].as_str(), item["textEdit"]["newText"].as_str()].into_iter().flatten() {
            assert!(!text.contains("${") && !text.contains("$1"), "{item}");
        }
    }
}
