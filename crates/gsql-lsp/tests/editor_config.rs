//! `gsql-lsp config`: the printed settings match what the server reads, and
//! the snippets agree with the integrations under `editors/`.

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::process::Command;

use gsql_lsp::editor_config::{Options, leaves, settings_json, snippets};
use gsql_lsp::features::{Config, KeywordCase};
use serde_json::{Value, json};

const EDITORS: [&str; 6] =
    ["neovim", "vscode", "helix", "zed", "emacs", "vim"];

fn repo_file(relative: &str) -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(relative);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

fn bodies(editor: &str, options: &Options) -> Vec<(String, String)> {
    snippets(editor, options, "gsql-lsp")
        .into_iter()
        .map(|s| (s.variant.to_string(), s.body))
        .collect()
}

fn body(editor: &str, variant: &str) -> String {
    bodies(editor, &Options::default())
        .into_iter()
        .find(|(v, _)| v == variant)
        .unwrap()
        .1
}

fn gsql_lsp(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_gsql-lsp"))
        .args(args)
        .output()
        .unwrap()
}

/// Setting paths the reader handles: every `flag(&["a", "b"])` call and the
/// `format.keywordCase` lookup in `Config::update`.
fn reader_paths() -> BTreeSet<String> {
    let source = repo_file("crates/gsql-lsp/src/features/mod.rs");
    let update = &source[source
        .find("pub fn update")
        .expect("Config::update")..];
    let update = &update[..update
        .find("\n    }\n}")
        .expect("end of update")];
    let mut paths = BTreeSet::new();
    for part in update.split("flag(&[").skip(1) {
        let keys = &part[..part.find(']').unwrap()];
        let keys: Vec<&str> = keys
            .split(',')
            .map(|k| k.trim().trim_matches('"'))
            .collect();
        paths.insert(keys.join("."));
    }
    assert!(
        update.contains(".get(\"format\")")
            && update.contains(".get(\"keywordCase\")")
    );
    paths.insert("format.keywordCase".into());
    paths
}

fn printed_paths() -> BTreeSet<String> {
    leaves(&settings_json(&Config::default()))
        .into_iter()
        .map(|(p, _)| p.join("."))
        .collect()
}

#[test]
fn printed_settings_cover_every_key_the_reader_handles() {
    assert_eq!(reader_paths(), printed_paths());
    // The flat `gsql.*` form of VS Code lists the same keys.
    let vscode: Value =
        serde_json::from_str(&body("vscode", "settings")).unwrap();
    let flat: BTreeSet<String> = vscode
        .as_object()
        .unwrap()
        .keys()
        .map(|k| k.strip_prefix("gsql.").unwrap().to_string())
        .collect();
    assert_eq!(flat, printed_paths());
}

#[test]
fn every_printed_key_is_read_by_the_server() {
    // Changing any one printed value changes the resulting Config.
    for (path, value) in leaves(&settings_json(&Config::default())) {
        let changed = match value {
            Value::Bool(b) => Value::Bool(!b),
            _ => json!("upper"),
        };
        let mut object = json!({});
        let mut cursor = &mut object;
        for key in &path {
            cursor = cursor
                .as_object_mut()
                .unwrap()
                .entry(key.clone())
                .or_insert(json!({}));
        }
        *cursor = changed;
        let mut config = Config::default();
        config.update(&object);
        assert_ne!(
            config,
            Config::default(),
            "{} is not read",
            path.join(".")
        );
    }
}

#[test]
fn printed_defaults_equal_config_default() {
    let printed = settings_json(&Config::default());
    // Start from a Config that differs in every field, then apply the printed defaults.
    let mut config = Config {
        semantic_tokens_lexical: true,
        diagnostics_unknown_types: false,
        diagnostics_unknown_attributes: false,
        diagnostics_undefined_names: false,
        diagnostics_unused: false,
        diagnostics_language_rules: false,
        diagnostics_float_equality: false,
        diagnostics_no_schema_notice: false,
        diagnostics_duplicate_definitions: false,
        diagnostics_style: false,
        format_keyword_case: KeywordCase::Upper,
        inlay_hints: false,
    };
    config.update(&printed);
    assert_eq!(config, Config::default());
    config.update(&json!({ "gsql": printed }));
    assert_eq!(config, Config::default());
}

#[test]
fn json_snippets_parse_and_hold_the_settings() {
    for options in [
        Options::default(),
        Options {
            settings_only: true,
            ..Options::default()
        },
        Options {
            absolute: true,
            ..Options::default()
        },
    ] {
        for (editor, variant) in
            [("vscode", "settings"), ("zed", "settings"), ("vim", "coc")]
        {
            let text = snippets(editor, &options, "/opt/gsql \"x\"/gsql-lsp")
                .into_iter()
                .find(|s| s.variant == variant)
                .unwrap()
                .body;
            let value: Value = serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{editor}: {e}\n{text}"));
            if options.absolute && !options.settings_only {
                assert!(
                    text.contains(r#"/opt/gsql \"x\"/gsql-lsp"#),
                    "{editor}"
                );
            }
            let defaults = settings_json(&Config::default());
            match (editor, options.settings_only) {
                ("zed", false) => assert_eq!(
                    value["lsp"]["gsql-lsp"]["initialization_options"],
                    defaults
                ),
                ("zed", true) => assert_eq!(value, defaults),
                ("vim", false) => {
                    let server = &value["languageserver"]["gsql"];
                    assert_eq!(
                        server["initializationOptions"]["gsql"],
                        defaults
                    );
                    assert_eq!(server["settings"]["gsql"], defaults);
                    assert_eq!(server["filetypes"], json!(["gsql"]));
                    assert_eq!(
                        server["rootPatterns"],
                        json!([".gsqlroot", ".git"])
                    );
                }
                ("vim", true) => {
                    assert_eq!(value["gsql"], defaults)
                }
                _ => {}
            }
        }
    }
}

#[test]
fn helix_snippet_is_structurally_valid_toml() {
    for options in [
        Options::default(),
        Options {
            settings_only: true,
            ..Options::default()
        },
    ] {
        let text = &bodies("helix", &options)[0].1;
        let mut tables = Vec::new();
        let mut keys: BTreeSet<String> = BTreeSet::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
        {
            if let Some(header) = line
                .strip_prefix("[[")
                .and_then(|l| l.strip_suffix("]]"))
                .or_else(|| {
                    line.strip_prefix('[')
                        .and_then(|l| l.strip_suffix(']'))
                })
            {
                assert!(
                    !header.is_empty()
                        && !header.contains('[')
                        && !header.contains(']'),
                    "{line}"
                );
                tables.push(header.to_string());
                continue;
            }
            let (key, value) = line
                .split_once(" = ")
                .unwrap_or_else(|| panic!("not key = value: {line}"));
            assert!(
                key.chars().all(|c| c.is_ascii_alphanumeric()
                    || c == '-'
                    || c == '_'),
                "{line}"
            );
            assert_eq!(
                value.matches('{').count(),
                value.matches('}').count(),
                "{line}"
            );
            assert_eq!(
                value.matches('[').count(),
                value.matches(']').count(),
                "{line}"
            );
            assert_eq!(value.matches('"').count() % 2, 0, "{line}");
            assert!(
                keys.insert(format!("{}/{key}", tables.last().unwrap())),
                "duplicate key: {line}"
            );
        }
        assert!(
            tables.contains(
                &"language-server.gsql-lsp.config.gsql".to_string()
            )
        );
        // Settings keys and values match the defaults, group by group.
        let mut from_text = serde_json::Map::new();
        for line in text.lines().filter(|l| l.contains(" = { ")) {
            let (key, value) = line.split_once(" = ").unwrap();
            if !["diagnostics", "format", "inlayHints", "semanticTokens"]
                .contains(&key)
            {
                continue;
            }
            // Inline tables here hold only bare keys and booleans/strings, which is JSON after quoting keys.
            let mut json = String::new();
            let mut word = String::new();
            let flush = |word: &mut String, json: &mut String, next: char| {
                if !word.is_empty() {
                    if next == '=' {
                        json.push_str(&format!("\"{word}\""))
                    } else {
                        json.push_str(word)
                    }
                    word.clear();
                }
            };
            let mut chars = value.chars().peekable();
            let mut in_string = false;
            while let Some(c) = chars.next() {
                if in_string {
                    json.push(c);
                    in_string = c != '"';
                } else if c == '"' {
                    in_string = true;
                    json.push(c);
                } else if c.is_ascii_alphanumeric() {
                    word.push(c);
                } else {
                    let next = chars
                        .clone()
                        .find(|c| *c != ' ')
                        .unwrap_or(' ');
                    flush(&mut word, &mut json, next);
                    json.push(if c == '=' { ':' } else { c });
                }
            }
            from_text.insert(
                key.to_string(),
                serde_json::from_str(&json)
                    .unwrap_or_else(|e| panic!("{json}: {e}")),
            );
        }
        assert_eq!(
            Value::Object(from_text),
            settings_json(&Config::default())
        );
    }
    let full = &bodies("helix", &Options::default())[0].1;
    assert!(
        full.contains("file-types = [\"gsql\", \"gsq\"]")
            && full.contains("roots = [\".gsqlroot\", \".git\"]")
    );
    assert!(
        full.contains("[language-server.gsql-lsp]\ncommand = \"gsql-lsp\"")
    );
}

#[test]
fn lua_elisp_and_vim_literals_are_balanced() {
    for editor in EDITORS {
        for (variant, text) in bodies(editor, &Options::default())
            .into_iter()
            .chain(bodies(
                editor,
                &Options {
                    settings_only: true,
                    ..Options::default()
                },
            ))
        {
            if variant == "coc"
                || ["vscode", "zed", "helix"].contains(&editor)
            {
                continue;
            }
            let strip = |t: &str| -> String {
                // Drop string contents and comments so only structure remains.
                let mut out = String::new();
                let mut quote: Option<char> = None;
                for line in t.lines() {
                    let line = line.trim_start();
                    if line.starts_with(";;") {
                        continue;
                    }
                    for c in line.chars() {
                        match quote {
                            Some(q) if c == q => quote = None,
                            Some(_) => {}
                            None if c == '\''
                                && !matches!(editor, "emacs")
                                || c == '"' =>
                            {
                                quote = Some(c)
                            }
                            None => out.push(c),
                        }
                    }
                    if editor != "emacs" {
                        quote = None;
                    }
                    out.push('\n');
                }
                out
            };
            let s = strip(&text);
            for (open, close) in [('{', '}'), ('(', ')'), ('[', ']')] {
                assert_eq!(
                    s.matches(open).count(),
                    s.matches(close).count(),
                    "{editor}/{variant}: {open}{close}\n{text}"
                );
            }
        }
    }
}

#[test]
fn snippets_agree_with_the_neovim_integration() {
    let lsp = repo_file("editors/neovim/lsp/gsql_lsp.lua");
    let plugin = body("neovim", "lsp-config");
    assert!(
        lsp.contains("filetypes = { 'gsql' }")
            && plugin.contains("filetypes = { 'gsql' }")
    );
    assert!(lsp.contains(
        "root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md', 'README' } }"
    ));
    assert!(plugin.contains(
        "root_markers = { { '.gsqlroot' }, { '.git' }, { 'README.md', 'README' } }"
    ));
    assert!(
        plugin.contains("vim.lsp.enable('gsql_lsp')")
            && plugin.contains("vim.lsp.config('gsql_lsp'")
    );
    assert!(
        repo_file("editors/neovim/ftdetect/gsql.lua")
            .contains("extension = { gsql = 'gsql', gsq = 'gsql' }")
    );
    assert!(plugin.contains("extension = { gsql = 'gsql', gsq = 'gsql' }"));
    // Without the plugin nothing else gives .gsql files a filetype, and the
    // server only starts for a filetype: the line must come with the config.
    let ft_then_config = "vim.filetype.add({ extension = { gsql = 'gsql', gsq = 'gsql' } })\nvim.lsp.config('gsql_lsp', {";
    assert!(plugin.starts_with(ft_then_config), "{plugin}");
    assert!(
        repo_file("editors/neovim/README.md").contains(ft_then_config),
        "editors/neovim/README.md"
    );

    // The settings table in the integration lists the same keys as the printed one.
    for path in printed_paths() {
        let leaf = path.rsplit('.').next().unwrap();
        assert!(
            lsp.contains(&format!("{leaf} =")),
            "{path} missing from editors/neovim/lsp/gsql_lsp.lua"
        );
        assert!(
            plugin.contains(&format!("{leaf} =")),
            "{path} missing from the lsp-config snippet"
        );
    }
    // The lazy spec goes through the plugin's own setup().
    let lazy = body("neovim", "lazy");
    assert!(
        lazy.contains("require('gsql').setup(")
            && lazy.contains("plugin.dir .. '/editors/neovim'")
    );
    assert!(
        repo_file("editors/neovim/lua/gsql/init.lua")
            .contains("function M.setup(")
    );
    assert!(lazy.contains("'abrahamchandy95/gsql-lsp'"));
}

#[test]
fn snippets_agree_with_the_vscode_manifest() {
    let manifest: Value =
        serde_json::from_str(&repo_file("editors/vscode/package.json"))
            .unwrap();
    let properties = manifest["contributes"]["configuration"]["properties"]
        .as_object()
        .unwrap();
    let declared: BTreeSet<String> = properties
        .keys()
        .filter(|k| {
            !k.starts_with("gsql.server.") && !k.starts_with("gsql.trace.")
        })
        .cloned()
        .collect();
    let printed: BTreeSet<String> = printed_paths()
        .into_iter()
        .map(|p| format!("gsql.{p}"))
        .collect();
    assert_eq!(declared, printed);
    let defaults = settings_json(&Config::default());
    for (path, value) in leaves(&defaults) {
        let key = format!("gsql.{}", path.join("."));
        assert_eq!(properties[&key]["default"], value, "{key}");
    }
    let absolute = snippets(
        "vscode",
        &Options {
            absolute: true,
            ..Options::default()
        },
        "/x/gsql-lsp",
    )
    .remove(0)
    .body;
    assert!(
        properties.contains_key("gsql.server.path")
            && absolute.contains("\"gsql.server.path\": \"/x/gsql-lsp\"")
    );
    let extensions = manifest["contributes"]["languages"][0]["extensions"]
        .as_array()
        .unwrap()
        .clone();
    assert_eq!(extensions, vec![json!(".gsql"), json!(".gsq")]);
}

#[test]
fn snippets_agree_with_the_other_integrations() {
    let helix = repo_file("editors/helix/languages.toml");
    let printed = body("helix", "languages");
    for line in [
        "file-types = [\"gsql\", \"gsq\"]",
        "roots = [\".gsqlroot\", \".git\"]",
        "language-servers = [\"gsql-lsp\"]",
        "command = \"gsql-lsp\"",
        "scope = \"source.gsql\"",
        "[language-server.gsql-lsp.config.gsql]",
    ] {
        assert!(helix.contains(line) && printed.contains(line), "{line}");
    }
    assert!(
        printed.contains("subpath = \"tree-sitter-gsql\"")
            && helix.contains("subpath = \"tree-sitter-gsql\"")
    );
    // Same setting groups, same values.
    for key in ["diagnostics", "format", "inlayHints", "semanticTokens"] {
        let line = helix
            .lines()
            .find(|l| l.starts_with(&format!("{key} = ")))
            .unwrap();
        assert!(
            printed.lines().any(|l| l
                .split_whitespace()
                .eq(line.split_whitespace())
                || l.starts_with(&format!("{key} = "))),
            "{key}"
        );
    }
    let zed = repo_file("editors/zed/languages/gsql/config.toml");
    assert!(zed.contains("path_suffixes = [\"gsql\", \"gsq\"]"));
    assert!(
        repo_file("editors/zed/extension.toml")
            .contains("[language_servers.gsql-lsp]")
    );
    assert!(
        repo_file("editors/zed/src/lib.rs")
            .contains("LspSettings::for_worktree(\"gsql-lsp\"")
    );
    let emacs = repo_file("editors/emacs/gsql-ts-mode.el");
    assert!(emacs.contains("'(gsql-ts-mode . (\"gsql-lsp\"))"));
    assert!(emacs.contains(r#"'("\\.gsql?\\'" . gsql-ts-mode)"#));
    assert!(
        body("emacs", "eglot").contains("'(gsql-ts-mode . (\"gsql-lsp\"))")
    );
    assert!(body("emacs", "lsp-mode").contains("(gsql-ts-mode . \"gsql\")"));
    assert!(
        repo_file("editors/vim/ftdetect/gsql.vim")
            .contains("*.gsql,*.gsq setfiletype gsql")
    );
    assert!(body("vim", "vim-lsp").contains("*.gsql,*.gsq setfiletype gsql"));
    assert!(body("vim", "vim9lsp").contains("'filetype': ['gsql']"));
}

#[test]
fn repository_is_configurable_in_one_place() {
    let options = Options {
        repo: Some("https://example.org/me/gsql.git"),
        ..Options::default()
    };
    assert!(
        bodies("helix", &options)[0]
            .1
            .contains("git = \"https://example.org/me/gsql.git\"")
    );
    assert!(
        bodies("neovim", &options)[0]
            .1
            .contains("'https://example.org/me/gsql.git'")
    );
    let github = Options {
        repo: Some("https://github.com/me/gsql"),
        ..Options::default()
    };
    assert!(
        bodies("neovim", &github)[0]
            .1
            .contains("'me/gsql'")
    );
    // The default appears only through the constant.
    assert!(
        bodies("neovim", &Options::default())[0]
            .1
            .contains(&format!("'{}'", "abrahamchandy95/gsql-lsp"))
    );
}

#[test]
fn cli_lists_prints_and_rejects() {
    let list = gsql_lsp(&["config", "--list"]);
    assert!(list.status.success());
    let list = String::from_utf8(list.stdout).unwrap();
    for editor in EDITORS {
        assert!(list.contains(editor), "{list}");
    }

    let out = gsql_lsp(&["config", "--settings"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["gsql"], settings_json(&Config::default()));

    let out = gsql_lsp(&["config", "nvim", "--settings"]);
    let text = String::from_utf8(out.stdout).unwrap();
    assert!(
        text.starts_with("{\n  gsql = {")
            && text.contains("keywordCase = 'preserve'"),
        "{text}"
    );

    // Snippets alone on stdout, instructions on stderr.
    let out = gsql_lsp(&["config", "helix"]);
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("languages.toml"));
    assert!(
        String::from_utf8(out.stdout)
            .unwrap()
            .starts_with("[[language]]")
    );

    // --absolute prints this very binary.
    let exe = env!("CARGO_BIN_EXE_gsql-lsp");
    let out = gsql_lsp(&["config", "zed", "--absolute"]);
    let value: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(value["lsp"]["gsql-lsp"]["binary"]["path"], json!(exe));

    let out = gsql_lsp(&["config", "vim", "--variant", "coc"]);
    assert!(serde_json::from_slice::<Value>(&out.stdout).is_ok());
    let all =
        String::from_utf8(gsql_lsp(&["config", "emacs", "--all"]).stdout)
            .unwrap();
    assert!(
        all.contains("=== emacs: eglot ===")
            && all.contains("=== emacs: lsp-mode ===")
    );

    for bad in [
        &["config"][..],
        &["config", "notepad"],
        &["config", "vim", "--variant", "x"],
        &["config", "vim", "--nope"],
    ] {
        let out = gsql_lsp(bad);
        assert_eq!(out.status.code(), Some(2), "{bad:?}");
        assert!(out.stdout.is_empty());
    }
}

#[test]
fn help_mentions_config() {
    let out = gsql_lsp(&["--help"]);
    let help = String::from_utf8(out.stdout).unwrap();
    assert!(
        help.contains("gsql-lsp config <editor>")
            && help.contains("config --list")
    );
}

#[test]
fn an_option_does_not_swallow_the_next_option() {
    let out = gsql_lsp(&["config", "vim", "--variant", "--all"]);
    assert!(!out.status.success());
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("--variant takes a name"),
        "{out:?}"
    );
}

#[test]
fn says_when_the_repository_is_a_placeholder() {
    let out = gsql_lsp(&["config", "neovim"]);
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("placeholder"),
        "{out:?}"
    );
    let out = gsql_lsp(&[
        "config",
        "neovim",
        "--repo",
        "https://github.com/me/gsql",
    ]);
    assert!(
        !String::from_utf8_lossy(&out.stderr).contains("placeholder"),
        "{out:?}"
    );
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("me/gsql"),
        "{out:?}"
    );
}
