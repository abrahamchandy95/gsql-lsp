//! `gsql-lsp config <editor>`: ready-to-paste editor configuration, generated
//! from the server's own settings so it cannot go stale.
//!
//! Standard output holds only the snippet; where to put it goes to standard
//! error, so `gsql-lsp config helix >> languages.toml` works.

use std::io::{self, Write};

use serde_json::{Map, Value, json};

use crate::features::{Config, KeywordCase};

/// Repository the snippets refer to (grammar sources, Neovim plugin). The one
/// place to change it; `--repo URL` overrides it per invocation.
pub const REPOSITORY: &str = "https://github.com/abrahamchandy95/gsql-lsp";

/// The server command when it is not given as an absolute path.
const COMMAND: &str = "gsql-lsp";

/// Project root markers for editors that have no better convention. Neovim
/// additionally falls back to a README, see `editors/neovim/lsp/gsql_lsp.lua`.
const ROOT_MARKERS: [&str; 2] = [".gsqlroot", ".git"];

/// Editor, its variants (the first is the default) and a description.
const EDITORS: &[(&str, &[&str], &str)] = &[
    ("neovim", &["lazy", "lsp-config"], "lazy.nvim spec; vim.lsp.config without the plugin"),
    ("vscode", &["settings"], "settings.json"),
    ("helix", &["languages"], "languages.toml"),
    ("zed", &["settings"], "settings.json"),
    ("emacs", &["eglot", "lsp-mode"], "Eglot; lsp-mode"),
    ("vim", &["vim-lsp", "vim9lsp", "coc"], "vim-lsp; yegappan/lsp; coc.nvim"),
];

pub const HELP: &str = "\
USAGE:
    gsql-lsp config --list
    gsql-lsp config --settings
    gsql-lsp config <editor> [--variant NAME | --all] [--settings] [--absolute] [--repo URL]

Editors: neovim, vscode, helix, zed, emacs, vim (`--list` shows the variants).
    --settings    only the server settings, with their defaults, in the editor's syntax
    --absolute    use the path of this executable instead of `gsql-lsp` from PATH
    --repo URL    repository URL used in snippets (default: the built-in placeholder)
";

/// Every setting the server reads, as the JSON object `Config::update` accepts
/// (without the optional `gsql` wrapper), holding the values of `config`.
pub fn settings_json(config: &Config) -> Value {
    // Exhaustive on purpose: a new `Config` field must be added here.
    let Config {
        semantic_tokens_lexical,
        diagnostics_unknown_types,
        diagnostics_unknown_attributes,
        diagnostics_undefined_names,
        diagnostics_unused,
        diagnostics_language_rules,
        diagnostics_float_equality,
        diagnostics_no_schema_notice,
        diagnostics_duplicate_definitions,
        diagnostics_style,
        format_keyword_case,
        inlay_hints,
    } = config;
    json!({
        "diagnostics": {
            "unknownTypes": diagnostics_unknown_types,
            "unknownAttributes": diagnostics_unknown_attributes,
            "undefinedNames": diagnostics_undefined_names,
            "unused": diagnostics_unused,
            "languageRules": diagnostics_language_rules,
            "floatEquality": diagnostics_float_equality,
            "noSchemaNotice": diagnostics_no_schema_notice,
            "duplicateDefinitions": diagnostics_duplicate_definitions,
            "style": diagnostics_style,
        },
        "format": { "keywordCase": match format_keyword_case {
            KeywordCase::Preserve => "preserve",
            KeywordCase::Upper => "upper",
            KeywordCase::Lower => "lower",
        } },
        "inlayHints": { "enabled": inlay_hints },
        "semanticTokens": { "lexical": semantic_tokens_lexical },
    })
}

fn defaults() -> Value {
    settings_json(&Config::default())
}

/// `{"diagnostics": {"unused": true}}` to `[(["diagnostics", "unused"], true)]`.
pub fn leaves(value: &Value) -> Vec<(Vec<String>, Value)> {
    fn walk(value: &Value, path: &mut Vec<String>, out: &mut Vec<(Vec<String>, Value)>) {
        match value {
            Value::Object(map) => {
                for (key, child) in map {
                    path.push(key.clone());
                    walk(child, path, out);
                    path.pop();
                }
            }
            leaf => out.push((path.clone(), leaf.clone())),
        }
    }
    let mut out = Vec::new();
    walk(value, &mut Vec::new(), &mut out);
    out
}

fn wrapped(value: Value) -> Value {
    json!({ "gsql": value })
}

// ---- value renderers -------------------------------------------------------

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("JSON serializes")
}

fn is_identifier(key: &str) -> bool {
    !key.is_empty()
        && !key.starts_with(|c: char| c.is_ascii_digit())
        && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn lua_string(s: &str) -> String {
    format!("'{}'", s.replace('\\', "\\\\").replace('\'', "\\'"))
}

fn lua(value: &Value, depth: usize) -> String {
    match value {
        Value::Object(map) => {
            let pad = "  ".repeat(depth + 1);
            let mut out = String::from("{\n");
            for (key, child) in map {
                let key = if is_identifier(key) { key.clone() } else { format!("[{}]", lua_string(key)) };
                out += &format!("{pad}{key} = {},\n", lua(child, depth + 1));
            }
            out + &"  ".repeat(depth) + "}"
        }
        Value::String(s) => lua_string(s),
        other => other.to_string(),
    }
}

fn toml_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

fn toml_key(key: &str) -> String {
    if !key.is_empty() && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-') {
        key.to_string()
    } else {
        toml_string(key)
    }
}

fn toml_inline(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let fields: Vec<String> =
                map.iter().map(|(k, v)| format!("{} = {}", toml_key(k), toml_inline(v))).collect();
            format!("{{ {} }}", fields.join(", "))
        }
        Value::String(s) => toml_string(s),
        other => other.to_string(),
    }
}

fn elisp_string(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// A plist as Eglot and lsp-mode take it: `(:a (:b t :c :json-false))`.
fn elisp_plist(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let fields: Vec<String> = map.iter().map(|(k, v)| format!(":{k} {}", elisp_plist(v))).collect();
            format!("({})", fields.join(" "))
        }
        Value::String(s) => elisp_string(s),
        Value::Bool(true) => "t".into(),
        Value::Bool(false) => ":json-false".into(),
        other => other.to_string(),
    }
}

fn vim_string(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// A Vim script dictionary (legacy script, so booleans are `v:true`/`v:false`).
fn vim_dict(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let fields: Vec<String> = map.iter().map(|(k, v)| format!("{}: {}", vim_string(k), vim_dict(v))).collect();
            format!("{{{}}}", fields.join(", "))
        }
        Value::String(s) => vim_string(s),
        Value::Bool(true) => "v:true".into(),
        Value::Bool(false) => "v:false".into(),
        other => other.to_string(),
    }
}

fn indent_tail(text: &str, spaces: usize) -> String {
    text.replace('\n', &format!("\n{}", " ".repeat(spaces)))
}

fn join_quoted(items: &[&str], quote: fn(&str) -> String) -> String {
    items.iter().map(|m| quote(m)).collect::<Vec<_>>().join(", ")
}

/// `https://github.com/owner/name(.git)` to `owner/name`, which lazy.nvim takes
/// as a GitHub shorthand; other URLs are kept whole.
fn repo_slug(repo: &str) -> String {
    let trimmed = repo.trim_end_matches('/').trim_end_matches(".git");
    trimmed.strip_prefix("https://github.com/").unwrap_or(repo).to_string()
}

// ---- snippets --------------------------------------------------------------

#[derive(Debug, Clone, Copy, Default)]
pub struct Options<'a> {
    pub variant: Option<&'a str>,
    pub all: bool,
    pub settings_only: bool,
    pub absolute: bool,
    pub repo: Option<&'a str>,
}

/// One snippet: `target` says where it goes, `body` is what to paste.
#[derive(Debug, PartialEq)]
pub struct Snippet {
    pub variant: &'static str,
    pub target: String,
    pub body: String,
}

fn snippet(variant: &'static str, target: &str, body: String) -> Snippet {
    Snippet { variant, target: target.to_string(), body }
}

/// The canonical editor name for an alias, or `None`.
pub fn canonical_editor(name: &str) -> Option<&'static str> {
    Some(match name.to_ascii_lowercase().as_str() {
        "neovim" | "nvim" => "neovim",
        "vscode" | "code" | "vs-code" => "vscode",
        "helix" | "hx" => "helix",
        "zed" => "zed",
        "emacs" => "emacs",
        "vim" => "vim",
        _ => return None,
    })
}

/// The snippets of `editor` (a canonical name), default variant first.
/// `command` is the server command or path.
pub fn snippets(editor: &str, options: &Options, command: &str) -> Vec<Snippet> {
    let settings = defaults();
    let repo = options.repo.unwrap_or(REPOSITORY);
    let only = options.settings_only;
    let mut out = Vec::new();
    match editor {
        "neovim" => {
            let table = lua(&wrapped(settings), 0);
            if only {
                out.push(snippet("lazy", "the `settings` field of the `lsp` option of setup()", table.clone()));
                out.push(snippet("lsp-config", "the `settings` field of vim.lsp.config('gsql_lsp', ...)", table));
                return out;
            }
            // Without --absolute the plugin finds `gsql-lsp` itself (PATH, then the usual install folders).
            let cmd = if options.absolute {
                format!("        cmd = {{ {} }},\n", lua_string(command))
            } else {
                String::new()
            };
            let body = format!(
                "{{\n  {},\n  config = function(plugin)\n    vim.opt.rtp:append(plugin.dir .. '/editors/neovim')\n    require('gsql').setup({{\n      lsp = {{\n{cmd}        settings = {},\n      }},\n    }})\n  end,\n}}",
                lua_string(&repo_slug(repo)),
                indent_tail(&table, 8)
            );
            out.push(snippet(
                "lazy",
                "a lazy.nvim spec file, e.g. ~/.config/nvim/lua/plugins/gsql.lua (inside the returned list)",
                body,
            ));
            let mut markers: Vec<String> = ROOT_MARKERS.iter().map(|m| format!("{{ {} }}", lua_string(m))).collect();
            markers.push(format!("{{ {} }}", join_quoted(&["README.md", "README"], lua_string)));
            let body = format!(
                "vim.filetype.add({{ extension = {{ gsql = 'gsql', gsq = 'gsql' }} }})\nvim.lsp.config('gsql_lsp', {{\n  cmd = {{ {} }},\n  filetypes = {{ 'gsql' }},\n  root_markers = {{ {} }},\n  settings = {},\n}})\nvim.lsp.enable('gsql_lsp')",
                lua_string(command),
                markers.join(", "),
                indent_tail(&table, 2)
            );
            out.push(snippet(
                "lsp-config",
                "init.lua (Neovim 0.11+; server only, no plugin: no highlighting or parser)",
                body,
            ));
        }
        "vscode" => {
            let mut flat = Map::new();
            if options.absolute && !only {
                flat.insert("gsql.server.path".into(), Value::String(command.to_string()));
            }
            for (path, value) in leaves(&settings) {
                flat.insert(format!("gsql.{}", path.join(".")), value);
            }
            out.push(snippet(
                "settings",
                "settings.json (user or workspace), merged into the top-level object",
                pretty(&Value::Object(flat)),
            ));
        }
        "helix" => {
            let mut groups = String::new();
            for (key, value) in settings.as_object().expect("settings object") {
                groups += &format!("{} = {}\n", toml_key(key), toml_inline(value));
            }
            let server_settings = format!("[language-server.gsql-lsp.config.gsql]\n{groups}");
            if only {
                out.push(snippet("languages", "~/.config/helix/languages.toml", server_settings));
                return out;
            }
            let body = format!(
                "[[language]]\nname = \"gsql\"\nscope = \"source.gsql\"\nfile-types = [\"gsql\", \"gsq\"]\ncomment-tokens = [\"//\", \"#\"]\nblock-comment-tokens = {{ start = \"/*\", end = \"*/\" }}\nindent = {{ tab-width = 4, unit = \"    \" }}\nroots = [{}]\nlanguage-servers = [\"gsql-lsp\"]\n\n[language-server.gsql-lsp]\ncommand = {}\n\n{server_settings}\n[[grammar]]\nname = \"gsql\"\nsource = {{ git = {}, rev = \"main\", subpath = \"tree-sitter-gsql\" }}",
                join_quoted(&ROOT_MARKERS, toml_string),
                toml_string(command),
                toml_string(repo),
            );
            out.push(snippet(
                "languages",
                "~/.config/helix/languages.toml; then run `hx --grammar fetch && hx --grammar build` and copy editors/helix/queries/gsql to ~/.config/helix/runtime/queries/gsql",
                body,
            ));
        }
        "zed" => {
            if only {
                out.push(snippet(
                    "settings",
                    "the `initialization_options` of `lsp.gsql-lsp` in settings.json",
                    pretty(&settings),
                ));
                return out;
            }
            let mut server = Map::new();
            if options.absolute {
                server.insert("binary".into(), json!({ "path": command }));
            }
            server.insert("initialization_options".into(), settings);
            let body = pretty(&json!({ "lsp": { "gsql-lsp": server } }));
            out.push(snippet(
                "settings",
                "Zed settings.json, merged into the top-level object (the extension in editors/zed must be installed)",
                body,
            ));
        }
        "emacs" => {
            let plist = elisp_plist(&wrapped(settings));
            if only {
                out.push(snippet(
                    "eglot",
                    "init.el, as the value of `eglot-workspace-configuration` (or in .dir-locals.el)",
                    format!("'{plist}"),
                ));
                out.push(snippet(
                    "lsp-mode",
                    "init.el, as the result of `:initialization-options`",
                    format!("'{plist}"),
                ));
                return out;
            }
            let cmd = elisp_string(command);
            let load = ";; (load \"/path/to/gsql-lsp/editors/emacs/gsql-ts-mode.el\")";
            let body = format!(
                "{load}\n(with-eval-after-load 'eglot\n  (add-to-list 'eglot-server-programs '(gsql-ts-mode . ({cmd}))))\n(setq-default eglot-workspace-configuration\n              '{plist})\n(add-hook 'gsql-ts-mode-hook #'eglot-ensure)"
            );
            out.push(snippet("eglot", "init.el, after loading gsql-ts-mode.el from editors/emacs", body));
            let body = format!(
                "{load}\n(with-eval-after-load 'lsp-mode\n  (add-to-list 'lsp-language-id-configuration '(gsql-ts-mode . \"gsql\"))\n  (lsp-register-client\n   (make-lsp-client\n    :new-connection (lsp-stdio-connection '({cmd}))\n    :activation-fn (lsp-activate-on \"gsql\")\n    :initialization-options (lambda () '{plist})\n    :server-id 'gsql-lsp)))\n(add-hook 'gsql-ts-mode-hook #'lsp-deferred)"
            );
            out.push(snippet("lsp-mode", "init.el, after loading gsql-ts-mode.el from editors/emacs", body));
        }
        "vim" => {
            let dict = vim_dict(&wrapped(settings.clone()));
            if only {
                out.push(snippet("vim-lsp", "the `initialization_options` of the server entry", dict.clone()));
                out.push(snippet("vim9lsp", "the `initializationOptions` of the server entry", dict));
                out.push(snippet(
                    "coc",
                    "the `initializationOptions` of the server in coc-settings.json",
                    pretty(&wrapped(settings)),
                ));
                return out;
            }
            let ft = "autocmd BufNewFile,BufRead *.gsql,*.gsq setfiletype gsql";
            let cmd = vim_string(command);
            let markers = join_quoted(&ROOT_MARKERS, vim_string);
            let body = format!(
                "{ft}\nfunction! s:GsqlRoot() abort\n  let l:dir = lsp#utils#find_nearest_parent_file_directory(lsp#utils#get_buffer_path(), [{markers}])\n  return empty(l:dir) ? expand('%:p:h') : l:dir\nendfunction\naugroup gsql_lsp\n  autocmd!\n  autocmd User lsp_setup call lsp#register_server({{\n        \\ 'name': 'gsql-lsp',\n        \\ 'cmd': {{server_info -> [{cmd}]}},\n        \\ 'allowlist': ['gsql'],\n        \\ 'root_uri': {{server_info -> lsp#utils#path_to_uri(s:GsqlRoot())}},\n        \\ 'initialization_options': {dict},\n        \\ }})\naugroup END"
            );
            out.push(snippet("vim-lsp", "vimrc (prabirshrestha/vim-lsp)", body));
            let body = format!(
                "{ft}\nautocmd User LspSetup call LspAddServer([{{\n      \\ 'name': 'gsql-lsp',\n      \\ 'filetype': ['gsql'],\n      \\ 'path': {cmd},\n      \\ 'args': [],\n      \\ 'initializationOptions': {dict},\n      \\ }}])"
            );
            out.push(snippet("vim9lsp", "vimrc (yegappan/lsp)", body));
            let body = pretty(&json!({
                "languageserver": { "gsql": {
                    "command": command,
                    "filetypes": ["gsql"],
                    "rootPatterns": ROOT_MARKERS,
                    "initializationOptions": wrapped(settings.clone()),
                    "settings": wrapped(settings),
                } }
            }));
            out.push(snippet("coc", "coc-settings.json (:CocConfig), merged into the top-level object; filetype detection needs the first line of the other variants", body));
        }
        _ => {}
    }
    out
}

// ---- command line ----------------------------------------------------------

/// Runs `gsql-lsp config ...` (`args` excludes the word `config`). Snippets go
/// to `out`, instructions to `err`. The error is a usage message.
/// The value of an option: present, and not the next option.
fn value<'a>(arg: Option<&'a String>, usage: &str) -> Result<&'a String, String> {
    arg.filter(|v| !v.starts_with('-')).ok_or_else(|| usage.to_string())
}

pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> Result<(), String> {
    let mut editor: Option<&'static str> = None;
    let mut options = Options::default();
    let mut list = false;
    let mut rest = args.iter();
    while let Some(arg) = rest.next() {
        match arg.as_str() {
            "--list" => list = true,
            "--settings" => options.settings_only = true,
            "--absolute" => options.absolute = true,
            "--all" => options.all = true,
            "--variant" => options.variant = Some(value(rest.next(), "--variant takes a name")?),
            "--repo" => options.repo = Some(value(rest.next(), "--repo takes a URL")?),
            flag if flag.starts_with('-') => return Err(format!("unknown option {flag}")),
            name => {
                if editor.is_some() {
                    return Err(format!("unexpected argument {name}"));
                }
                editor = Some(
                    canonical_editor(name)
                        .ok_or_else(|| format!("unknown editor {name} (see `gsql-lsp config --list`)"))?,
                );
            }
        }
    }
    let io_err = |e: io::Error| e.to_string();
    if list {
        for (name, variants, what) in EDITORS {
            writeln!(out, "{name}\tvariants: {}\t{what}", variants.join(", ")).map_err(io_err)?;
        }
        return Ok(());
    }
    let Some(editor) = editor else {
        if options.settings_only {
            writeln!(out, "{}", pretty(&wrapped(defaults()))).map_err(io_err)?;
            return Ok(());
        }
        return Err("config takes an editor name, --list or --settings".into());
    };
    let command = if options.absolute {
        std::env::current_exe().map_err(|e| format!("cannot find this executable: {e}"))?.to_string_lossy().into_owned()
    } else {
        COMMAND.to_string()
    };
    let all = snippets(editor, &options, &command);
    let names: Vec<&str> = all.iter().map(|s| s.variant).collect();
    let chosen: Vec<&Snippet> = match options.variant {
        Some(name) => {
            let found: Vec<&Snippet> = all.iter().filter(|s| s.variant == name).collect();
            if found.is_empty() {
                return Err(format!("{editor} has no variant {name} (variants: {})", names.join(", ")));
            }
            found
        }
        None if options.all => all.iter().collect(),
        None => all.iter().take(1).collect(),
    };
    for (i, s) in chosen.iter().enumerate() {
        if options.all && chosen.len() > 1 {
            if i > 0 {
                writeln!(out).map_err(io_err)?;
            }
            writeln!(out, "=== {editor}: {} ===", s.variant).map_err(io_err)?;
        }
        writeln!(err, "{editor} ({}): put this in {}", s.variant, s.target).map_err(io_err)?;
        writeln!(out, "{}", s.body.trim_end()).map_err(io_err)?;
    }
    if options.repo.is_none() && matches!(editor, "neovim" | "helix") {
        writeln!(
            err,
            "note: the repository URL {REPOSITORY} is a placeholder until the project is published; pass --repo URL"
        )
        .map_err(io_err)?;
    }
    if names.len() > 1 && options.variant.is_none() && !options.all {
        writeln!(err, "other variants: {} (--variant NAME, or --all)", names[1..].join(", ")).map_err(io_err)?;
    }
    Ok(())
}
